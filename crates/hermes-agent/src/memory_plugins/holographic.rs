//! Holographic memory provider plugin.
//!
//! SQLite-backed structured fact storage with entity resolution, trust scoring,
//! and keyword-based retrieval. Rust port of the Python holographic memory plugin.
//!
//! The Python version uses HRR (Holographic Reduced Representations) with numpy
//! for vector algebra. This Rust port uses keyword/FTS-based retrieval as a
//! simplified but fully functional alternative.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

// ---------------------------------------------------------------------------
// SQLite schema
// ---------------------------------------------------------------------------

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS facts (
    fact_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    content         TEXT NOT NULL UNIQUE,
    category        TEXT DEFAULT 'general',
    tags            TEXT DEFAULT '',
    trust_score     REAL DEFAULT 0.5,
    retrieval_count INTEGER DEFAULT 0,
    helpful_count   INTEGER DEFAULT 0,
    created_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS entities (
    entity_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    entity_type TEXT DEFAULT 'unknown',
    aliases     TEXT DEFAULT '',
    created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS fact_entities (
    fact_id   INTEGER REFERENCES facts(fact_id),
    entity_id INTEGER REFERENCES entities(entity_id),
    PRIMARY KEY (fact_id, entity_id)
);

CREATE INDEX IF NOT EXISTS idx_facts_trust    ON facts(trust_score DESC);
CREATE INDEX IF NOT EXISTS idx_facts_category ON facts(category);
CREATE INDEX IF NOT EXISTS idx_entities_name  ON entities(name);
";

// Trust adjustment constants
const HELPFUL_DELTA: f64 = 0.05;
const UNHELPFUL_DELTA: f64 = -0.10;

fn clamp_trust(v: f64) -> f64 {
    v.max(0.0).min(1.0)
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn fact_store_schema() -> Value {
    json!({
        "name": "fact_store",
        "description": "Deep structured memory with keyword search and trust scoring.\n\nACTIONS:\n• add — Store a fact.\n• search — Keyword lookup.\n• update — Modify a fact.\n• remove — Delete a fact.\n• list — Browse facts by category/trust.",
        "parameters": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "search", "update", "remove", "list"]},
                "content": {"type": "string", "description": "Fact content (required for 'add')."},
                "query": {"type": "string", "description": "Search query (for 'search')."},
                "fact_id": {"type": "integer", "description": "Fact ID for update/remove."},
                "category": {"type": "string", "enum": ["user_pref", "project", "tool", "general"]},
                "tags": {"type": "string", "description": "Comma-separated tags."},
                "trust_delta": {"type": "number", "description": "Trust adjustment for 'update'."},
                "min_trust": {"type": "number", "description": "Minimum trust filter (default: 0.3)."},
                "limit": {"type": "integer", "description": "Max results (default: 10)."}
            },
            "required": ["action"]
        }
    })
}

fn fact_feedback_schema() -> Value {
    json!({
        "name": "fact_feedback",
        "description": "Rate a fact after using it. Mark 'helpful' if accurate, 'unhelpful' if outdated.",
        "parameters": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["helpful", "unhelpful"]},
                "fact_id": {"type": "integer", "description": "The fact ID to rate."}
            },
            "required": ["action", "fact_id"]
        }
    })
}

// ---------------------------------------------------------------------------
// Entity extraction (simple regex-like patterns)
// ---------------------------------------------------------------------------

fn extract_entities(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    let mut add = |name: &str| {
        let s = name.trim().to_string();
        let lower = s.to_lowercase();
        if !s.is_empty() && !seen.contains(&lower) {
            seen.insert(lower);
            result.push(s);
        }
    };

    // Capitalized multi-word phrases (e.g. "John Doe")
    let re_cap = regex::Regex::new(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)+)\b").unwrap();
    for cap in re_cap.captures_iter(text) {
        add(&cap[1]);
    }

    // Double-quoted terms
    let re_dq = regex::Regex::new(r#""([^"]+)""#).unwrap();
    for cap in re_dq.captures_iter(text) {
        add(&cap[1]);
    }

    // Single-quoted terms
    let re_sq = regex::Regex::new(r"'([^']+)'").unwrap();
    for cap in re_sq.captures_iter(text) {
        add(&cap[1]);
    }

    result
}

// ---------------------------------------------------------------------------
// HolographicMemoryPlugin
// ---------------------------------------------------------------------------

/// Holographic memory with structured facts, entity resolution, and trust scoring.
pub struct HolographicMemoryPlugin {
    conn: Mutex<Option<Connection>>,
    db_path: Mutex<Option<PathBuf>>,
    default_trust: f64,
    min_trust: f64,
    session_id: Mutex<String>,
}

impl HolographicMemoryPlugin {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
            db_path: Mutex::new(None),
            default_trust: 0.5,
            min_trust: 0.3,
            session_id: Mutex::new(String::new()),
        }
    }

    fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> Option<R> {
        let guard = self.conn.lock().ok()?;
        guard.as_ref().map(f)
    }

    fn add_fact(&self, content: &str, category: &str, tags: &str) -> Result<i64, String> {
        let guard = self.conn.lock().map_err(|e| e.to_string())?;
        let conn = guard.as_ref().ok_or("Not initialized")?;

        let content = content.trim();
        if content.is_empty() {
            return Err("content must not be empty".into());
        }

        match conn.execute(
            "INSERT INTO facts (content, category, tags, trust_score) VALUES (?1, ?2, ?3, ?4)",
            params![content, category, tags, self.default_trust],
        ) {
            Ok(_) => {
                let fact_id = conn.last_insert_rowid();
                for name in extract_entities(content) {
                    let entity_id = self.resolve_entity(conn, &name);
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO fact_entities (fact_id, entity_id) VALUES (?1, ?2)",
                        params![fact_id, entity_id],
                    );
                }
                Ok(fact_id)
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                let existing: i64 = conn
                    .query_row(
                        "SELECT fact_id FROM facts WHERE content = ?1",
                        params![content],
                        |row| row.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(existing)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn search_facts(
        &self,
        query: &str,
        _category: Option<&str>,
        min_trust: f64,
        limit: usize,
    ) -> Vec<Value> {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let conn = match guard.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };

        let query_lower = query.to_lowercase();
        let keywords: Vec<&str> = query_lower.split_whitespace().collect();
        if keywords.is_empty() {
            return Vec::new();
        }

        // Build individual LIKE patterns and merge results.
        let mut results = Vec::new();

        // Search for each keyword and merge results.
        for keyword in &keywords {
            let pattern = format!("%{}%", keyword);
            let mut stmt = match conn.prepare(
                "SELECT fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at \
                 FROM facts WHERE trust_score >= ?1 AND (LOWER(content) LIKE ?2 OR LOWER(tags) LIKE ?2) ORDER BY trust_score DESC LIMIT ?3"
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let rows = stmt.query_map(params![min_trust, pattern, limit as i64], |row| {
                Ok(json!({
                    "fact_id": row.get::<_, i64>(0)?,
                    "content": row.get::<_, String>(1)?,
                    "category": row.get::<_, String>(2)?,
                    "tags": row.get::<_, String>(3)?,
                    "trust_score": row.get::<_, f64>(4)?,
                    "retrieval_count": row.get::<_, i64>(5)?,
                    "helpful_count": row.get::<_, i64>(6)?,
                    "created_at": row.get::<_, String>(7)?,
                    "updated_at": row.get::<_, String>(8)?,
                }))
            });

            if let Ok(rows) = rows {
                for row in rows.flatten() {
                    let fid = row["fact_id"].as_i64().unwrap_or(0);
                    if !results
                        .iter()
                        .any(|r: &Value| r["fact_id"].as_i64() == Some(fid))
                    {
                        results.push(row);
                    }
                }
            }
        }

        // Update retrieval counts
        let ids: Vec<i64> = results
            .iter()
            .filter_map(|r| r["fact_id"].as_i64())
            .collect();
        for id in ids {
            let _ = conn.execute(
                "UPDATE facts SET retrieval_count = retrieval_count + 1 WHERE fact_id = ?1",
                params![id],
            );
        }

        results.truncate(limit);
        results
    }

    fn resolve_entity(&self, conn: &Connection, name: &str) -> i64 {
        if let Ok(id) = conn.query_row(
            "SELECT entity_id FROM entities WHERE name LIKE ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        ) {
            return id;
        }

        let _ = conn.execute("INSERT INTO entities (name) VALUES (?1)", params![name]);
        conn.last_insert_rowid()
    }

    fn record_feedback(&self, fact_id: i64, helpful: bool) -> Result<Value, String> {
        let guard = self.conn.lock().map_err(|e| e.to_string())?;
        let conn = guard.as_ref().ok_or("Not initialized")?;

        let (old_trust, old_helpful): (f64, i64) = conn
            .query_row(
                "SELECT trust_score, helpful_count FROM facts WHERE fact_id = ?1",
                params![fact_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| format!("fact_id {} not found", fact_id))?;

        let delta = if helpful {
            HELPFUL_DELTA
        } else {
            UNHELPFUL_DELTA
        };
        let new_trust = clamp_trust(old_trust + delta);
        let helpful_inc: i64 = if helpful { 1 } else { 0 };

        conn.execute(
            "UPDATE facts SET trust_score = ?1, helpful_count = helpful_count + ?2, updated_at = CURRENT_TIMESTAMP WHERE fact_id = ?3",
            params![new_trust, helpful_inc, fact_id],
        ).map_err(|e| e.to_string())?;

        Ok(json!({
            "fact_id": fact_id,
            "old_trust": old_trust,
            "new_trust": new_trust,
            "helpful_count": old_helpful + helpful_inc,
        }))
    }

    fn fact_count(&self) -> i64 {
        self.with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get::<_, i64>(0))
                .unwrap_or(0)
        })
        .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// MemoryProviderPlugin implementation
// ---------------------------------------------------------------------------

impl MemoryProviderPlugin for HolographicMemoryPlugin {
    fn name(&self) -> &str {
        "holographic"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let db_path = PathBuf::from(hermes_home).join("memory_store.db");
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match Connection::open(&db_path) {
            Ok(conn) => {
                let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
                if let Err(e) = conn.execute_batch(SCHEMA) {
                    tracing::warn!("Holographic memory schema init failed: {}", e);
                    return;
                }
                *self.db_path.lock().unwrap() = Some(db_path);
                *self.conn.lock().unwrap() = Some(conn);
                *self.session_id.lock().unwrap() = session_id.to_string();
                tracing::info!(
                    "Holographic memory plugin initialized for session {}",
                    session_id
                );
            }
            Err(e) => {
                tracing::warn!("Failed to open holographic memory DB: {}", e);
            }
        }
    }

    fn system_prompt_block(&self) -> String {
        let total = self.fact_count();
        if total == 0 {
            "# Holographic Memory\n\
             Active. Empty fact store — proactively add facts the user would expect you to remember.\n\
             Use fact_store(action='add') to store durable structured facts.\n\
             Use fact_feedback to rate facts after using them."
                .to_string()
        } else {
            format!(
                "# Holographic Memory\n\
                 Active. {} facts stored with entity resolution and trust scoring.\n\
                 Use fact_store to search or add facts.\n\
                 Use fact_feedback to rate facts after using them.",
                total
            )
        }
    }

    fn prefetch(&self, query: &str, _session_id: &str) -> String {
        if query.trim().is_empty() {
            return String::new();
        }
        let results = self.search_facts(query, None, self.min_trust, 5);
        if results.is_empty() {
            return String::new();
        }
        let mut lines = Vec::new();
        for r in &results {
            let trust = r["trust_score"].as_f64().unwrap_or(0.0);
            let content = r["content"].as_str().unwrap_or("");
            lines.push(format!("- [{:.1}] {}", trust, content));
        }
        format!("## Holographic Memory\n{}", lines.join("\n"))
    }

    fn sync_turn(&self, _user_content: &str, _assistant_content: &str, _session_id: &str) {
        // Holographic memory stores explicit facts via tools, not auto-sync
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![fact_store_schema(), fact_feedback_schema()]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        match tool_name {
            "fact_store" => self.handle_fact_store(args),
            "fact_feedback" => self.handle_fact_feedback(args),
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_session_end(&self, _messages: &[Value]) {
        tracing::debug!("Holographic memory session end");
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if action == "add" && !content.is_empty() {
            let category = if target == "user" {
                "user_pref"
            } else {
                "general"
            };
            let _ = self.add_fact(content, category, "");
        }
    }

    fn shutdown(&self) {
        *self.conn.lock().unwrap() = None;
        tracing::debug!("Holographic memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "db_path", "description": "SQLite database path"},
            {"key": "default_trust", "description": "Default trust score for new facts", "default": "0.5"},
            {"key": "min_trust_threshold", "description": "Minimum trust for search results", "default": "0.3"}
        ]))
    }

    fn save_config(&self, _config: &Value) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tool handler implementations
// ---------------------------------------------------------------------------

impl HolographicMemoryPlugin {
    fn handle_fact_store(&self, args: &Value) -> String {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return json!({"error": "Missing required argument: action"}).to_string(),
        };

        match action {
            "add" => {
                let content = match args.get("content").and_then(|c| c.as_str()) {
                    Some(c) => c,
                    None => {
                        return json!({"error": "Missing required argument: content"}).to_string()
                    }
                };
                let category = args
                    .get("category")
                    .and_then(|c| c.as_str())
                    .unwrap_or("general");
                let tags = args.get("tags").and_then(|t| t.as_str()).unwrap_or("");
                match self.add_fact(content, category, tags) {
                    Ok(id) => json!({"fact_id": id, "status": "added"}).to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "search" => {
                let query = match args.get("query").and_then(|q| q.as_str()) {
                    Some(q) => q,
                    None => {
                        return json!({"error": "Missing required argument: query"}).to_string()
                    }
                };
                let category = args.get("category").and_then(|c| c.as_str());
                let min_trust = args
                    .get("min_trust")
                    .and_then(|m| m.as_f64())
                    .unwrap_or(self.min_trust);
                let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
                let results = self.search_facts(query, category, min_trust, limit);
                json!({"results": results, "count": results.len()}).to_string()
            }
            "update" => {
                let fact_id = match args.get("fact_id").and_then(|f| f.as_i64()) {
                    Some(id) => id,
                    None => {
                        return json!({"error": "Missing required argument: fact_id"}).to_string()
                    }
                };
                let guard = match self.conn.lock() {
                    Ok(g) => g,
                    Err(e) => return json!({"error": e.to_string()}).to_string(),
                };
                let conn = match guard.as_ref() {
                    Some(c) => c,
                    None => return json!({"error": "Not initialized"}).to_string(),
                };

                let mut assignments = vec!["updated_at = CURRENT_TIMESTAMP".to_string()];
                let mut sql_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                if let Some(content) = args.get("content").and_then(|c| c.as_str()) {
                    assignments.push(format!("content = ?{}", sql_params.len() + 1));
                    sql_params.push(Box::new(content.to_string()));
                }
                if let Some(tags) = args.get("tags").and_then(|t| t.as_str()) {
                    assignments.push(format!("tags = ?{}", sql_params.len() + 1));
                    sql_params.push(Box::new(tags.to_string()));
                }
                if let Some(cat) = args.get("category").and_then(|c| c.as_str()) {
                    assignments.push(format!("category = ?{}", sql_params.len() + 1));
                    sql_params.push(Box::new(cat.to_string()));
                }
                if let Some(delta) = args.get("trust_delta").and_then(|d| d.as_f64()) {
                    let old_trust: f64 = conn
                        .query_row(
                            "SELECT trust_score FROM facts WHERE fact_id = ?1",
                            params![fact_id],
                            |row| row.get(0),
                        )
                        .unwrap_or(0.5);
                    let new_trust = clamp_trust(old_trust + delta);
                    assignments.push(format!("trust_score = ?{}", sql_params.len() + 1));
                    sql_params.push(Box::new(new_trust));
                }

                let sql = format!(
                    "UPDATE facts SET {} WHERE fact_id = ?{}",
                    assignments.join(", "),
                    sql_params.len() + 1
                );
                sql_params.push(Box::new(fact_id));

                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    sql_params.iter().map(|p| p.as_ref()).collect();
                match conn.execute(&sql, param_refs.as_slice()) {
                    Ok(n) => json!({"updated": n > 0}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "remove" => {
                let fact_id = match args.get("fact_id").and_then(|f| f.as_i64()) {
                    Some(id) => id,
                    None => {
                        return json!({"error": "Missing required argument: fact_id"}).to_string()
                    }
                };
                let guard = match self.conn.lock() {
                    Ok(g) => g,
                    Err(e) => return json!({"error": e.to_string()}).to_string(),
                };
                let conn = match guard.as_ref() {
                    Some(c) => c,
                    None => return json!({"error": "Not initialized"}).to_string(),
                };
                let _ = conn.execute(
                    "DELETE FROM fact_entities WHERE fact_id = ?1",
                    params![fact_id],
                );
                let n = conn
                    .execute("DELETE FROM facts WHERE fact_id = ?1", params![fact_id])
                    .unwrap_or(0);
                json!({"removed": n > 0}).to_string()
            }
            "list" => {
                let guard = match self.conn.lock() {
                    Ok(g) => g,
                    Err(e) => return json!({"error": e.to_string()}).to_string(),
                };
                let conn = match guard.as_ref() {
                    Some(c) => c,
                    None => return json!({"error": "Not initialized"}).to_string(),
                };
                let category = args.get("category").and_then(|c| c.as_str());
                let min_trust = args
                    .get("min_trust")
                    .and_then(|m| m.as_f64())
                    .unwrap_or(0.0);
                let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as i64;

                let (sql, param_list): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                    if let Some(cat) = category {
                        (
                        "SELECT fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at \
                         FROM facts WHERE trust_score >= ?1 AND category = ?2 ORDER BY trust_score DESC LIMIT ?3".to_string(),
                        vec![Box::new(min_trust) as Box<dyn rusqlite::types::ToSql>, Box::new(cat.to_string()), Box::new(limit)],
                    )
                    } else {
                        (
                        "SELECT fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at \
                         FROM facts WHERE trust_score >= ?1 ORDER BY trust_score DESC LIMIT ?2".to_string(),
                        vec![Box::new(min_trust) as Box<dyn rusqlite::types::ToSql>, Box::new(limit)],
                    )
                    };

                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    param_list.iter().map(|p| p.as_ref()).collect();
                let mut stmt = match conn.prepare(&sql) {
                    Ok(s) => s,
                    Err(e) => return json!({"error": e.to_string()}).to_string(),
                };

                let rows = stmt.query_map(param_refs.as_slice(), |row| {
                    Ok(json!({
                        "fact_id": row.get::<_, i64>(0)?,
                        "content": row.get::<_, String>(1)?,
                        "category": row.get::<_, String>(2)?,
                        "tags": row.get::<_, String>(3)?,
                        "trust_score": row.get::<_, f64>(4)?,
                        "retrieval_count": row.get::<_, i64>(5)?,
                        "helpful_count": row.get::<_, i64>(6)?,
                    }))
                });

                let facts: Vec<Value> = rows.map(|r| r.flatten().collect()).unwrap_or_default();
                json!({"facts": facts, "count": facts.len()}).to_string()
            }
            _ => json!({"error": format!("Unknown action: {}", action)}).to_string(),
        }
    }

    fn handle_fact_feedback(&self, args: &Value) -> String {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return json!({"error": "Missing required argument: action"}).to_string(),
        };
        let fact_id = match args.get("fact_id").and_then(|f| f.as_i64()) {
            Some(id) => id,
            None => return json!({"error": "Missing required argument: fact_id"}).to_string(),
        };
        let helpful = action == "helpful";
        match self.record_feedback(fact_id, helpful) {
            Ok(result) => result.to_string(),
            Err(e) => json!({"error": e}).to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_name() {
        let plugin = HolographicMemoryPlugin::new();
        assert_eq!(plugin.name(), "holographic");
    }

    #[test]
    fn test_plugin_is_available() {
        let plugin = HolographicMemoryPlugin::new();
        assert!(plugin.is_available());
    }

    #[test]
    fn test_tool_schemas() {
        let plugin = HolographicMemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 2);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"fact_store"));
        assert!(names.contains(&"fact_feedback"));
    }

    #[test]
    fn test_initialize_and_add_fact() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let result = plugin.add_fact("User prefers dark mode", "user_pref", "ui,theme");
        assert!(result.is_ok());
        let fact_id = result.unwrap();
        assert!(fact_id > 0);

        assert_eq!(plugin.fact_count(), 1);
    }

    #[test]
    fn test_search_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let _ = plugin.add_fact("User prefers dark mode", "user_pref", "ui");
        let _ = plugin.add_fact("Project uses Rust", "project", "tech");

        let results = plugin.search_facts("dark mode", None, 0.0, 10);
        assert!(!results.is_empty());
        assert!(results[0]["content"]
            .as_str()
            .unwrap()
            .contains("dark mode"));
    }

    #[test]
    fn test_fact_feedback() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let fact_id = plugin.add_fact("Test fact", "general", "").unwrap();
        let result = plugin.record_feedback(fact_id, true).unwrap();
        assert!(result["new_trust"].as_f64().unwrap() > 0.5);
    }

    #[test]
    fn test_extract_entities() {
        let entities = extract_entities("John Doe works at Acme Corp using 'Python'");
        assert!(entities.contains(&"John Doe".to_string()));
        assert!(entities.contains(&"Acme Corp".to_string()));
        assert!(entities.contains(&"Python".to_string()));
    }

    #[test]
    fn test_deduplicate_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let id1 = plugin.add_fact("Same fact", "general", "").unwrap();
        let id2 = plugin.add_fact("Same fact", "general", "").unwrap();
        assert_eq!(id1, id2);
        assert_eq!(plugin.fact_count(), 1);
    }
}
