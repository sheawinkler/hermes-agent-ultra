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

use rusqlite::{params, params_from_iter, Connection, Row};
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

const FTS_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts
    USING fts5(content, tags, content=facts, content_rowid=fact_id);

CREATE TRIGGER IF NOT EXISTS facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, content, tags)
    VALUES (new.fact_id, new.content, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS facts_ad AFTER DELETE ON facts BEGIN
    DELETE FROM facts_fts WHERE rowid = old.fact_id;
END;

CREATE TRIGGER IF NOT EXISTS facts_au AFTER UPDATE ON facts BEGIN
    DELETE FROM facts_fts WHERE rowid = old.fact_id;
    INSERT INTO facts_fts(rowid, content, tags)
    VALUES (new.fact_id, new.content, new.tags);
END;
";

// Trust adjustment constants
const HELPFUL_DELTA: f64 = 0.05;
const UNHELPFUL_DELTA: f64 = -0.10;

const FTS_STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "don",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

fn clamp_trust(v: f64) -> f64 {
    v.max(0.0).min(1.0)
}

fn is_fts5_unavailable(error: &rusqlite::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("no such module")
        || message.contains("unknown tokenizer")
        || message.contains("fts5")
}

fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn tokenize_query(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter_map(|token| {
            let cleaned = token
                .trim_matches(|c: char| ".,;:!?\"'()[]{}#@<>".contains(c))
                .to_ascii_lowercase();
            (!cleaned.is_empty()).then_some(cleaned)
        })
        .collect()
}

fn sanitize_fts5_query(raw: &str) -> Option<String> {
    let mut terms = Vec::new();
    for token in tokenize_query(raw) {
        let cleaned = token
            .chars()
            .filter(|c| !matches!(c, '"' | '(' | ')' | '*' | '^' | ':' | '-' | '+'))
            .collect::<String>();
        if cleaned.len() < 2 || FTS_STOPWORDS.contains(&cleaned.as_str()) {
            continue;
        }
        terms.push(format!("\"{}\"", cleaned.replace('"', "\"\"")));
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn jaccard_similarity(left: &[String], right: &[String]) -> f64 {
    let left: std::collections::HashSet<&str> = left.iter().map(String::as_str).collect();
    let right: std::collections::HashSet<&str> = right.iter().map(String::as_str).collect();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn row_to_fact(row: &Row<'_>) -> rusqlite::Result<Value> {
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
    fts_enabled: Mutex<bool>,
    db_path: Mutex<Option<PathBuf>>,
    default_trust: f64,
    min_trust: f64,
    session_id: Mutex<String>,
}

impl HolographicMemoryPlugin {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
            fts_enabled: Mutex::new(false),
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

    fn ensure_fts_schema(conn: &Connection) -> Result<bool, String> {
        match conn.execute_batch(FTS_SCHEMA) {
            Ok(()) => {
                if let Err(err) =
                    conn.execute("INSERT INTO facts_fts(facts_fts) VALUES('rebuild')", [])
                {
                    tracing::debug!("Holographic memory FTS rebuild skipped: {}", err);
                }
                Ok(true)
            }
            Err(err) if is_fts5_unavailable(&err) => {
                tracing::warn!(
                    "SQLite FTS5 unavailable for holographic memory; falling back to LIKE search: {}",
                    err
                );
                Ok(false)
            }
            Err(err) => Err(err.to_string()),
        }
    }

    fn search_facts(
        &self,
        query: &str,
        category: Option<&str>,
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
        if query.trim().is_empty() || limit == 0 {
            return Vec::new();
        }

        let fts_enabled = self.fts_enabled.lock().map(|g| *g).unwrap_or(false);
        let mut results = if fts_enabled {
            self.search_facts_fts(conn, query, category, min_trust, limit * 3)
        } else {
            Vec::new()
        };

        if results.is_empty() {
            results = self.search_facts_like(conn, query, category, min_trust, limit * 3);
        }

        let query_tokens = tokenize_query(query);
        for fact in &mut results {
            let content_tokens = tokenize_query(fact["content"].as_str().unwrap_or_default());
            let tag_tokens = tokenize_query(fact["tags"].as_str().unwrap_or_default());
            let mut all_tokens = content_tokens;
            all_tokens.extend(tag_tokens);
            let jaccard = jaccard_similarity(&query_tokens, &all_tokens);
            let fts_rank = fact["fts_rank"].as_f64().unwrap_or(0.5);
            let trust = fact["trust_score"].as_f64().unwrap_or(0.0);
            fact["score"] = json!(((0.6 * fts_rank) + (0.4 * jaccard)) * trust);
        }
        results.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b["trust_score"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .partial_cmp(&a["trust_score"].as_f64().unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        results.truncate(limit);

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
        results
    }

    fn search_facts_fts(
        &self,
        conn: &Connection,
        query: &str,
        category: Option<&str>,
        min_trust: f64,
        limit: usize,
    ) -> Vec<Value> {
        let Some(match_query) = sanitize_fts5_query(query) else {
            return Vec::new();
        };
        let mut values = vec![
            rusqlite::types::Value::Text(match_query),
            rusqlite::types::Value::Real(min_trust),
        ];
        let category_clause = if let Some(category) = category {
            values.push(rusqlite::types::Value::Text(category.to_string()));
            " AND f.category = ?3"
        } else {
            ""
        };
        values.push(rusqlite::types::Value::Integer(limit as i64));
        let limit_placeholder = values.len();
        let sql = format!(
            "SELECT f.fact_id, f.content, f.category, f.tags, f.trust_score, \
                    f.retrieval_count, f.helpful_count, f.created_at, f.updated_at, \
                    facts_fts.rank \
             FROM facts_fts \
             JOIN facts f ON f.fact_id = facts_fts.rowid \
             WHERE facts_fts MATCH ?1 AND f.trust_score >= ?2{} \
             ORDER BY facts_fts.rank, f.trust_score DESC \
             LIMIT ?{}",
            category_clause, limit_placeholder
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(params_from_iter(values.iter()), |row| {
            let mut fact = row_to_fact(row)?;
            let raw_rank = row.get::<_, f64>(9).unwrap_or(0.0).abs();
            fact["fts_rank"] = json!(1.0 / (1.0 + raw_rank));
            Ok(fact)
        }) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        rows.flatten().collect()
    }

    fn search_facts_like(
        &self,
        conn: &Connection,
        query: &str,
        category: Option<&str>,
        min_trust: f64,
        limit: usize,
    ) -> Vec<Value> {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        let mut keywords = tokenize_query(query);
        if keywords.is_empty() {
            keywords.push(query.trim().to_ascii_lowercase());
        }
        for keyword in keywords {
            let pattern = format!("%{}%", escape_like(&keyword));
            let mut values = vec![
                rusqlite::types::Value::Real(min_trust),
                rusqlite::types::Value::Text(pattern),
            ];
            let category_clause = if let Some(category) = category {
                values.push(rusqlite::types::Value::Text(category.to_string()));
                " AND category = ?3"
            } else {
                ""
            };
            values.push(rusqlite::types::Value::Integer(limit as i64));
            let limit_placeholder = values.len();
            let sql = format!(
                "SELECT fact_id, content, category, tags, trust_score, retrieval_count, \
                        helpful_count, created_at, updated_at \
                 FROM facts \
                 WHERE trust_score >= ?1 \
                   AND (LOWER(content) LIKE ?2 ESCAPE '\\' OR LOWER(tags) LIKE ?2 ESCAPE '\\'){} \
                 ORDER BY trust_score DESC \
                 LIMIT ?{}",
                category_clause, limit_placeholder
            );
            let mut stmt = match conn.prepare(&sql) {
                Ok(stmt) => stmt,
                Err(_) => continue,
            };
            let rows = match stmt.query_map(params_from_iter(values.iter()), row_to_fact) {
                Ok(rows) => rows,
                Err(_) => continue,
            };
            for mut row in rows.flatten() {
                let fact_id = row["fact_id"].as_i64().unwrap_or_default();
                if seen.insert(fact_id) {
                    row["fts_rank"] = json!(0.25);
                    results.push(row);
                }
            }
        }
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
                let fts_enabled = match Self::ensure_fts_schema(&conn) {
                    Ok(enabled) => enabled,
                    Err(e) => {
                        tracing::warn!("Holographic memory FTS schema init failed: {}", e);
                        false
                    }
                };
                *self.db_path.lock().unwrap() = Some(db_path);
                *self.conn.lock().unwrap() = Some(conn);
                *self.fts_enabled.lock().unwrap() = fts_enabled;
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
        *self.fts_enabled.lock().unwrap() = false;
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
    fn test_search_facts_sanitizes_natural_language_fts_query() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let _ = plugin.add_fact(
            "Deployment rollback failed because the gateway token cache was stale",
            "project",
            "deploy,rollback",
        );

        let results = plugin.search_facts(
            "what happened with the deployment rollback?",
            Some("project"),
            0.0,
            10,
        );

        assert_eq!(results.len(), 1);
        assert!(results[0]["content"]
            .as_str()
            .unwrap()
            .contains("Deployment rollback"));
        assert!(results[0]["score"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn test_search_facts_handles_fts_special_characters_with_like_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let _ = plugin.add_fact(
            "C++ build notes mention foo-bar and simulate.p2.test.ts",
            "project",
            "c++,foo-bar,simulate.p2.test.ts",
        );

        let results = plugin.search_facts("C++ foo-bar simulate.p2.test.ts", None, 0.0, 10);

        assert_eq!(results.len(), 1);
        assert!(results[0]["content"]
            .as_str()
            .unwrap()
            .contains("C++ build"));
    }

    #[test]
    fn test_search_facts_honors_category_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let _ = plugin.add_fact("Rust gateway deploy note", "project", "gateway");
        let _ = plugin.add_fact("Rust personal preference note", "user_pref", "gateway");

        let results = plugin.search_facts("Rust gateway", Some("user_pref"), 0.0, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["category"].as_str(), Some("user_pref"));
    }

    #[test]
    fn test_fact_store_search_uses_sanitized_search_path() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());

        let _ = plugin.handle_tool_call(
            "fact_store",
            &json!({
                "action": "add",
                "content": "Natural recall should find the allocator regression",
                "category": "project",
                "tags": "allocator,regression"
            }),
        );
        let output = plugin.handle_tool_call(
            "fact_store",
            &json!({
                "action": "search",
                "query": "can you recall the allocator regression?",
                "category": "project"
            }),
        );
        let parsed: Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["count"].as_u64(), Some(1));
        assert!(parsed["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains("allocator regression"));
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

    #[test]
    fn test_shutdown_drops_sqlite_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = HolographicMemoryPlugin::new();
        plugin.initialize("test-session", tmp.path().to_str().unwrap());
        assert!(plugin.with_conn(|conn| conn.is_autocommit()).is_some());

        plugin.shutdown();

        assert!(plugin.with_conn(|conn| conn.is_autocommit()).is_none());
        assert_eq!(plugin.fact_count(), 0);
    }
}
