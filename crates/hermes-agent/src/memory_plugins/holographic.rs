//! Holographic memory provider plugin.
//!
//! SQLite-backed structured fact storage with entity resolution, trust scoring,
//! and keyword-based retrieval. Rust port of the Python holographic memory plugin.
//!
//! The Python version uses HRR (Holographic Reduced Representations) with numpy
//! for vector algebra. This Rust port uses keyword/FTS-based retrieval as a
//! simplified but fully functional alternative.

use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use rusqlite::{params, params_from_iter, Connection, Row, TransactionBehavior};
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

// ---------------------------------------------------------------------------
// SQLite schema
// ---------------------------------------------------------------------------

const HOLOGRAPHIC_SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS facts (
    fact_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_key     TEXT NOT NULL,
    content         TEXT NOT NULL,
    category        TEXT DEFAULT 'general',
    tags            TEXT DEFAULT '',
    trust_score     REAL DEFAULT 0.5,
    retrieval_count INTEGER DEFAULT 0,
    helpful_count   INTEGER DEFAULT 0,
    created_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(subject_key, content)
);

CREATE TABLE IF NOT EXISTS entities (
    entity_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_key     TEXT NOT NULL,
    name            TEXT NOT NULL,
    entity_type     TEXT DEFAULT 'unknown',
    aliases         TEXT DEFAULT '',
    created_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(subject_key, name)
);

CREATE TABLE IF NOT EXISTS fact_entities (
    subject_key TEXT NOT NULL,
    fact_id     INTEGER NOT NULL REFERENCES facts(fact_id) ON DELETE CASCADE,
    entity_id   INTEGER NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    PRIMARY KEY (subject_key, fact_id, entity_id)
);

CREATE INDEX IF NOT EXISTS idx_facts_subject_trust
    ON facts(subject_key, trust_score DESC);
CREATE INDEX IF NOT EXISTS idx_facts_subject_category
    ON facts(subject_key, category);
CREATE INDEX IF NOT EXISTS idx_facts_subject_updated
    ON facts(subject_key, updated_at);
CREATE INDEX IF NOT EXISTS idx_entities_subject_name
    ON entities(subject_key, name);
CREATE INDEX IF NOT EXISTS idx_fact_entities_subject_fact
    ON fact_entities(subject_key, fact_id);
";

const FTS_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts
    USING fts5(subject_key UNINDEXED, content, tags);

CREATE TRIGGER IF NOT EXISTS facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, subject_key, content, tags)
    VALUES (new.fact_id, new.subject_key, new.content, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS facts_ad AFTER DELETE ON facts BEGIN
    DELETE FROM facts_fts WHERE rowid = old.fact_id AND subject_key = old.subject_key;
END;

CREATE TRIGGER IF NOT EXISTS facts_au AFTER UPDATE ON facts BEGIN
    DELETE FROM facts_fts WHERE rowid = old.fact_id AND subject_key = old.subject_key;
    INSERT INTO facts_fts(rowid, subject_key, content, tags)
    VALUES (new.fact_id, new.subject_key, new.content, new.tags);
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

type SharedHolographicConnection = Arc<Mutex<Connection>>;
type HolographicConnectionRegistry = HashMap<PathBuf, Weak<Mutex<Connection>>>;

fn shared_holographic_connections() -> &'static Mutex<HolographicConnectionRegistry> {
    static CONNECTIONS: LazyLock<Mutex<HolographicConnectionRegistry>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    &CONNECTIONS
}

fn canonical_holographic_database_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    path.parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|| path.to_path_buf())
}

fn acquire_holographic_connection(
    db_path: &Path,
) -> Result<
    (
        SharedHolographicConnection,
        HolographicMigrationReport,
        bool,
        PathBuf,
    ),
    String,
> {
    let canonical_path = canonical_holographic_database_path(db_path);
    let mut registry = shared_holographic_connections().lock();
    registry.retain(|_, connection| connection.strong_count() != 0);

    if let Some(connection) = registry.get(&canonical_path).and_then(Weak::upgrade) {
        let fts_enabled = {
            let conn = connection.lock();
            sqlite_table_exists(&conn, "facts_fts")?
        };
        return Ok((
            connection,
            HolographicMigrationReport {
                schema_version: HOLOGRAPHIC_SCHEMA_VERSION,
                ..HolographicMigrationReport::default()
            },
            fts_enabled,
            canonical_path,
        ));
    }

    let mut conn = Connection::open(&canonical_path)
        .map_err(|error| format!("open holographic memory database: {error}"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(|error| format!("enable holographic WAL: {error}"))?;
    let (migration_report, fts_enabled) = migrate_holographic_schema(&mut conn)?;
    let connection = Arc::new(Mutex::new(conn));
    registry.insert(canonical_path.clone(), Arc::downgrade(&connection));
    Ok((connection, migration_report, fts_enabled, canonical_path))
}

fn release_holographic_connection(path: &Path, connection: SharedHolographicConnection) {
    let mut registry = shared_holographic_connections().lock();
    if Arc::strong_count(&connection) == 1
        && registry
            .get(path)
            .and_then(Weak::upgrade)
            .is_some_and(|registered| Arc::ptr_eq(&registered, &connection))
    {
        registry.remove(path);
    }
}

fn is_fts5_unavailable(error: &rusqlite::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("no such module")
        || message.contains("unknown tokenizer")
        || message.contains("fts5")
}

#[derive(Debug, Default, Clone, Copy)]
struct HolographicMigrationReport {
    schema_version: i64,
    quarantined_facts: i64,
    quarantined_entities: i64,
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(|error| error.to_string())
}

fn sqlite_table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    if !sqlite_table_exists(conn, table)? {
        return Ok(false);
    }
    let table = match table {
        "facts" | "entities" | "fact_entities" | "facts_fts" => table,
        _ => return Err(format!("unsupported SQLite table lookup: {table}")),
    };
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| error.to_string())?;
    let mut rows = statement.query([]).map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let name = row.get::<_, String>(1).map_err(|error| error.to_string())?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_table_row_count(conn: &Connection, table: &str) -> Result<i64, String> {
    let table = match table {
        "facts" | "entities" => table,
        _ => return Err(format!("unsupported SQLite count table: {table}")),
    };
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|error| error.to_string())
}

fn drop_holographic_fts_objects(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        DROP TRIGGER IF EXISTS facts_ai;
        DROP TRIGGER IF EXISTS facts_ad;
        DROP TRIGGER IF EXISTS facts_au;
        DROP TABLE IF EXISTS facts_fts;
        ",
    )
    .map_err(|error| error.to_string())
}

fn backfill_holographic_fts(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "
        INSERT INTO facts_fts(rowid, subject_key, content, tags)
        SELECT fact_id, subject_key, content, tags
        FROM facts
        ",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn migrate_holographic_schema(
    conn: &mut Connection,
) -> Result<(HolographicMigrationReport, bool), String> {
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| error.to_string())?;

    let has_facts = sqlite_table_exists(conn, "facts")?;
    let scoped_facts = !has_facts || sqlite_table_has_column(conn, "facts", "subject_key")?;
    let mut report = HolographicMigrationReport {
        schema_version: HOLOGRAPHIC_SCHEMA_VERSION,
        ..HolographicMigrationReport::default()
    };

    if has_facts && !scoped_facts {
        report.quarantined_facts = sqlite_table_row_count(conn, "facts")?;
        if sqlite_table_exists(conn, "entities")? {
            report.quarantined_entities = sqlite_table_row_count(conn, "entities")?;
        }
        for legacy_table in [
            "facts_legacy_quarantine_v1",
            "entities_legacy_quarantine_v1",
            "fact_entities_legacy_quarantine_v1",
        ] {
            if sqlite_table_exists(conn, legacy_table)? {
                return Err(format!(
                    "refusing to overwrite existing quarantined legacy table {legacy_table}"
                ));
            }
        }

        let has_entities = sqlite_table_exists(conn, "entities")?;
        let has_fact_entities = sqlite_table_exists(conn, "fact_entities")?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        drop_holographic_fts_objects(&transaction)?;
        if has_fact_entities {
            transaction
                .execute_batch(
                    "ALTER TABLE fact_entities RENAME TO fact_entities_legacy_quarantine_v1;",
                )
                .map_err(|error| error.to_string())?;
        }
        if has_entities {
            transaction
                .execute_batch("ALTER TABLE entities RENAME TO entities_legacy_quarantine_v1;")
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute_batch("ALTER TABLE facts RENAME TO facts_legacy_quarantine_v1;")
            .map_err(|error| error.to_string())?;
        transaction
            .execute_batch(SCHEMA)
            .map_err(|error| error.to_string())?;
        transaction
            .pragma_update(None, "user_version", HOLOGRAPHIC_SCHEMA_VERSION)
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
    } else {
        conn.execute_batch(SCHEMA)
            .map_err(|error| error.to_string())?;
        conn.pragma_update(None, "user_version", HOLOGRAPHIC_SCHEMA_VERSION)
            .map_err(|error| error.to_string())?;
    }

    let fts_ready = sqlite_table_has_column(conn, "facts_fts", "subject_key")?;
    if fts_ready {
        return Ok((report, true));
    }

    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    drop_holographic_fts_objects(&transaction)?;
    transaction.commit().map_err(|error| error.to_string())?;

    match conn.execute_batch(FTS_SCHEMA) {
        Ok(()) => {
            backfill_holographic_fts(conn)?;
            Ok((report, true))
        }
        Err(error) if is_fts5_unavailable(&error) => {
            tracing::warn!(
                "SQLite FTS5 unavailable for holographic memory; falling back to LIKE search: {error}"
            );
            Ok((report, false))
        }
        Err(error) => Err(error.to_string()),
    }
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
    conn: Mutex<Option<SharedHolographicConnection>>,
    fts_enabled: Mutex<bool>,
    db_path: Mutex<Option<PathBuf>>,
    migration_report: Mutex<HolographicMigrationReport>,
    default_trust: f64,
    min_trust: f64,
}

impl HolographicMemoryPlugin {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
            fts_enabled: Mutex::new(false),
            db_path: Mutex::new(None),
            migration_report: Mutex::new(HolographicMigrationReport::default()),
            default_trust: 0.5,
            min_trust: 0.3,
        }
    }

    fn shared_connection(&self) -> Option<SharedHolographicConnection> {
        self.conn.lock().clone()
    }

    fn release_connection(&self) {
        let connection = self.conn.lock().take();
        let db_path = self.db_path.lock().take();
        if let (Some(connection), Some(db_path)) = (connection, db_path) {
            release_holographic_connection(&db_path, connection);
        }
        *self.fts_enabled.lock() = false;
    }

    fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> Option<R> {
        let connection = self.shared_connection()?;
        let conn = connection.lock();
        Some(f(&conn))
    }

    fn add_fact(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        content: &str,
        category: &str,
        tags: &str,
    ) -> Result<i64, String> {
        let connection = self.shared_connection().ok_or("Not initialized")?;
        let mut conn = connection.lock();
        let content = content.trim();
        if content.is_empty() {
            return Err("content must not be empty".to_string());
        }
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        match transaction.execute(
            "
            INSERT INTO facts (subject_key, content, category, tags, trust_score)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![filter.as_str(), content, category, tags, self.default_trust],
        ) {
            Ok(_) => {
                let fact_id = transaction.last_insert_rowid();
                for name in extract_entities(content) {
                    let entity_id = Self::resolve_entity(&transaction, filter, &name)?;
                    transaction
                        .execute(
                            "
                            INSERT OR IGNORE INTO fact_entities (subject_key, fact_id, entity_id)
                            VALUES (?1, ?2, ?3)
                            ",
                            params![filter.as_str(), fact_id, entity_id],
                        )
                        .map_err(|error| error.to_string())?;
                }
                transaction.commit().map_err(|error| error.to_string())?;
                Ok(fact_id)
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                let existing = transaction
                    .query_row(
                        "
                        SELECT fact_id FROM facts
                        WHERE subject_key = ?1 AND content = ?2
                        ",
                        params![filter.as_str(), content],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())?;
                transaction.commit().map_err(|error| error.to_string())?;
                Ok(existing)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn search_facts(
        &self,
        filter: &MemorySubjectStorageKey,
        query: &str,
        category: Option<&str>,
        min_trust: f64,
        limit: usize,
    ) -> Vec<Value> {
        let Some(connection) = self.shared_connection() else {
            return Vec::new();
        };
        let mut conn = connection.lock();
        if query.trim().is_empty() || limit == 0 {
            return Vec::new();
        }

        let fts_enabled = *self.fts_enabled.lock();
        let mut results = if fts_enabled {
            self.search_facts_fts(&conn, filter, query, category, min_trust, limit * 3)
        } else {
            Vec::new()
        };
        if results.is_empty() {
            results = self.search_facts_like(&conn, filter, query, category, min_trust, limit * 3);
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
        results.sort_by(|left, right| {
            right["score"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&left["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    right["trust_score"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .partial_cmp(&left["trust_score"].as_f64().unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        results.truncate(limit);

        if !results.is_empty() {
            let transaction = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(transaction) => transaction,
                Err(error) => {
                    tracing::warn!("Holographic retrieval count transaction failed: {error}");
                    return results;
                }
            };
            for id in results
                .iter()
                .filter_map(|result| result["fact_id"].as_i64())
            {
                if let Err(error) = transaction.execute(
                    "
                    UPDATE facts
                    SET retrieval_count = retrieval_count + 1
                    WHERE fact_id = ?1 AND subject_key = ?2
                    ",
                    params![id, filter.as_str()],
                ) {
                    tracing::warn!("Holographic retrieval count update failed: {error}");
                }
            }
            if let Err(error) = transaction.commit() {
                tracing::warn!("Holographic retrieval count commit failed: {error}");
            }
        }
        results
    }

    fn search_facts_fts(
        &self,
        conn: &Connection,
        filter: &MemorySubjectStorageKey,
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
            rusqlite::types::Value::Text(filter.as_str().to_string()),
            rusqlite::types::Value::Real(min_trust),
        ];
        let category_clause = if let Some(category) = category {
            values.push(rusqlite::types::Value::Text(category.to_string()));
            " AND f.category = ?4"
        } else {
            ""
        };
        values.push(rusqlite::types::Value::Integer(limit as i64));
        let limit_placeholder = values.len();
        let sql = format!(
            "
            SELECT f.fact_id, f.content, f.category, f.tags, f.trust_score,
                   f.retrieval_count, f.helpful_count, f.created_at, f.updated_at,
                   facts_fts.rank
            FROM facts_fts
            JOIN facts f
              ON f.fact_id = facts_fts.rowid
             AND f.subject_key = facts_fts.subject_key
            WHERE facts_fts MATCH ?1
              AND facts_fts.subject_key = ?2
              AND f.subject_key = ?2
              AND f.trust_score >= ?3{}
            ORDER BY facts_fts.rank, f.trust_score DESC
            LIMIT ?{}
            ",
            category_clause, limit_placeholder
        );
        let mut statement = match conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = match statement.query_map(params_from_iter(values.iter()), |row| {
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
        filter: &MemorySubjectStorageKey,
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
                rusqlite::types::Value::Text(filter.as_str().to_string()),
                rusqlite::types::Value::Real(min_trust),
                rusqlite::types::Value::Text(pattern),
            ];
            let category_clause = if let Some(category) = category {
                values.push(rusqlite::types::Value::Text(category.to_string()));
                " AND category = ?4"
            } else {
                ""
            };
            values.push(rusqlite::types::Value::Integer(limit as i64));
            let limit_placeholder = values.len();
            let sql = format!(
                "
                SELECT fact_id, content, category, tags, trust_score, retrieval_count,
                       helpful_count, created_at, updated_at
                FROM facts
                WHERE subject_key = ?1
                  AND trust_score >= ?2
                  AND (
                    LOWER(content) LIKE ?3 ESCAPE '\\'
                    OR LOWER(tags) LIKE ?3 ESCAPE '\\'
                  ){}
                ORDER BY trust_score DESC
                LIMIT ?{}
                ",
                category_clause, limit_placeholder
            );
            let mut statement = match conn.prepare(&sql) {
                Ok(statement) => statement,
                Err(_) => continue,
            };
            let rows = match statement.query_map(params_from_iter(values.iter()), row_to_fact) {
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

    fn resolve_entity(
        conn: &Connection,
        filter: &MemorySubjectStorageKey,
        name: &str,
    ) -> Result<i64, String> {
        if let Ok(id) = conn.query_row(
            "
            SELECT entity_id FROM entities
            WHERE subject_key = ?1 AND name = ?2 COLLATE NOCASE
            ",
            params![filter.as_str(), name],
            |row| row.get::<_, i64>(0),
        ) {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO entities (subject_key, name) VALUES (?1, ?2)",
            params![filter.as_str(), name],
        )
        .map_err(|error| error.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    fn record_feedback(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        fact_id: i64,
        helpful: bool,
    ) -> Result<Value, String> {
        let connection = self.shared_connection().ok_or("Not initialized")?;
        let mut conn = connection.lock();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let (old_trust, old_helpful): (f64, i64) = transaction
            .query_row(
                "
                SELECT trust_score, helpful_count FROM facts
                WHERE fact_id = ?1 AND subject_key = ?2
                ",
                params![fact_id, filter.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| format!("fact_id {fact_id} not found"))?;
        let delta = if helpful {
            HELPFUL_DELTA
        } else {
            UNHELPFUL_DELTA
        };
        let new_trust = clamp_trust(old_trust + delta);
        let helpful_inc: i64 = i64::from(helpful);
        transaction
            .execute(
                "
                UPDATE facts
                SET trust_score = ?1, helpful_count = helpful_count + ?2,
                    updated_at = CURRENT_TIMESTAMP
                WHERE fact_id = ?3 AND subject_key = ?4
                ",
                params![new_trust, helpful_inc, fact_id, filter.as_str()],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(json!({
            "fact_id": fact_id,
            "old_trust": old_trust,
            "new_trust": new_trust,
            "helpful_count": old_helpful + helpful_inc,
        }))
    }

    fn fact_count(&self, filter: &MemorySubjectStorageKey) -> i64 {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM facts WHERE subject_key = ?1",
                params![filter.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
        })
        .unwrap_or(0)
    }

    fn subject_residue_count(&self, filter: &MemorySubjectStorageKey) -> Result<u64, String> {
        let connection = self.shared_connection().ok_or("Not initialized")?;
        let conn = connection.lock();
        let mut total = 0_u64;
        for table in ["facts", "entities", "fact_entities"] {
            let count = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE subject_key = ?1"),
                    params![filter.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            total = total.saturating_add(count.max(0) as u64);
        }
        if sqlite_table_exists(&conn, "facts_fts")? {
            let count = conn
                .query_row(
                    "SELECT COUNT(*) FROM facts_fts WHERE subject_key = ?1",
                    params![filter.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?;
            total = total.saturating_add(count.max(0) as u64);
        }
        Ok(total)
    }

    fn purge_subject(&self, filter: &MemorySubjectStorageKey) -> Result<u64, String> {
        let connection = self.shared_connection().ok_or("Not initialized")?;
        let mut conn = connection.lock();
        let fts_exists = sqlite_table_exists(&conn, "facts_fts")?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut deleted = 0_u64;
        for statement in [
            "DELETE FROM fact_entities WHERE subject_key = ?1",
            "DELETE FROM facts WHERE subject_key = ?1",
            "DELETE FROM entities WHERE subject_key = ?1",
        ] {
            let changed = transaction
                .execute(statement, params![filter.as_str()])
                .map_err(|error| error.to_string())?;
            deleted = deleted.saturating_add(changed as u64);
        }
        if fts_exists {
            let changed = transaction
                .execute(
                    "DELETE FROM facts_fts WHERE subject_key = ?1",
                    params![filter.as_str()],
                )
                .map_err(|error| error.to_string())?;
            deleted = deleted.saturating_add(changed as u64);
        }
        transaction.commit().map_err(|error| error.to_string())?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .map_err(|error| error.to_string())?;
        Ok(deleted)
    }

    fn apply_retention(
        &self,
        filter: &MemorySubjectStorageKey,
        policy: &MemoryRetentionPolicy,
    ) -> Result<u64, String> {
        let connection = self.shared_connection().ok_or("Not initialized")?;
        let mut conn = connection.lock();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut deleted = 0_u64;
        if let Some(max_age_days) = policy.max_age_days {
            let age = format!("-{max_age_days} days");
            let changed = transaction
                .execute(
                    "
                    DELETE FROM facts
                    WHERE subject_key = ?1
                      AND created_at < datetime('now', ?2)
                    ",
                    params![filter.as_str(), age],
                )
                .map_err(|error| error.to_string())?;
            deleted = deleted.saturating_add(changed as u64);
        }
        if let Some(max_records) = policy.max_records {
            let changed = transaction
                .execute(
                    "
                    DELETE FROM facts
                    WHERE subject_key = ?1
                      AND fact_id IN (
                        SELECT fact_id FROM facts
                        WHERE subject_key = ?1
                        ORDER BY updated_at DESC, fact_id DESC
                        LIMIT -1 OFFSET ?2
                      )
                    ",
                    params![filter.as_str(), max_records as i64],
                )
                .map_err(|error| error.to_string())?;
            deleted = deleted.saturating_add(changed as u64);
        }
        transaction
            .execute(
                "
                DELETE FROM fact_entities
                WHERE subject_key = ?1
                  AND NOT EXISTS (
                    SELECT 1 FROM facts WHERE facts.fact_id = fact_entities.fact_id
                  )
                ",
                params![filter.as_str()],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "
                DELETE FROM entities
                WHERE subject_key = ?1
                  AND NOT EXISTS (
                    SELECT 1 FROM fact_entities
                    WHERE fact_entities.entity_id = entities.entity_id
                  )
                ",
                params![filter.as_str()],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|error| error.to_string())?;
        Ok(deleted)
    }
}

impl Drop for HolographicMemoryPlugin {
    fn drop(&mut self) {
        self.release_connection();
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

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        let _ = subject;
        let db_path = PathBuf::from(hermes_home).join("memory_store.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create holographic memory directory: {error}"))?;
        }
        self.release_connection();
        let (conn, migration_report, fts_enabled, canonical_path) =
            acquire_holographic_connection(&db_path)?;
        *self.db_path.lock() = Some(canonical_path);
        *self.conn.lock() = Some(conn);
        *self.fts_enabled.lock() = fts_enabled;
        *self.migration_report.lock() = migration_report;
        tracing::info!(
            "Holographic memory initialized (schema_version={}, quarantined_facts={}, quarantined_entities={}, fts_enabled={})",
            migration_report.schema_version,
            migration_report.quarantined_facts,
            migration_report.quarantined_entities,
            fts_enabled
        );
        let _ = filter;
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        "# Holographic Memory\n\
         Active. Use fact_store to search or add subject-scoped facts and \
         fact_feedback to rate facts after using them."
            .to_string()
    }

    fn prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) -> String {
        let _ = subject;
        if query.trim().is_empty() {
            return String::new();
        }
        let results = self.search_facts(filter, query, None, self.min_trust, 5);
        if results.is_empty() {
            return String::new();
        }
        let lines = results
            .iter()
            .map(|result| {
                let trust = result["trust_score"].as_f64().unwrap_or(0.0);
                let content = result["content"].as_str().unwrap_or("");
                format!("- [{trust:.1}] {content}")
            })
            .collect::<Vec<_>>();
        format!("## Holographic Memory\n{}", lines.join("\n"))
    }

    fn sync_turn(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _user_content: &str,
        _assistant_content: &str,
    ) {
        // Holographic memory stores explicit facts via tools, not auto-sync.
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![fact_store_schema(), fact_feedback_schema()]
    }

    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        match tool_name {
            "fact_store" => self.handle_fact_store(subject, filter, args),
            "fact_feedback" => self.handle_fact_feedback(subject, filter, args),
            _ => json!({"error": format!("Unknown tool: {tool_name}")}).to_string(),
        }
    }

    fn on_session_end(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _messages: &[Value],
    ) {
        tracing::debug!("Holographic memory session end");
    }

    fn on_memory_write(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        action: &str,
        target: &str,
        content: &str,
    ) {
        if action == "add" && !content.is_empty() {
            let category = if target == "user" {
                "user_pref"
            } else {
                "general"
            };
            if let Err(error) = self.add_fact(subject, filter, content, category, "") {
                tracing::warn!("Holographic memory mirror failed: {error}");
            }
        }
    }

    fn export_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        let connection = self.shared_connection().ok_or_else(|| {
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Export)
        })?;
        let mut conn = connection.lock();
        // The immediate transaction freezes this complete ordered inventory
        // before it is copied into an immutable, paginated snapshot.
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                tracing::warn!("Holographic export transaction failed: {error}");
                MemoryProviderGovernanceError::failed(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                )
            })?;
        let records = {
            let mut statement = transaction
                .prepare(
                    "SELECT fact_id, content, category, tags, trust_score, retrieval_count,
                            helpful_count, created_at, updated_at
                     FROM facts WHERE subject_key = ?1 ORDER BY fact_id ASC",
                )
                .map_err(|error| {
                    tracing::warn!("Holographic export prepare failed: {error}");
                    MemoryProviderGovernanceError::failed(
                        self.name(),
                        MemoryGovernanceOperation::Export,
                    )
                })?;
            let rows = statement
                .query_map(params![filter.as_str()], row_to_fact)
                .map_err(|error| {
                    tracing::warn!("Holographic export query failed: {error}");
                    MemoryProviderGovernanceError::failed(
                        self.name(),
                        MemoryGovernanceOperation::Export,
                    )
                })?;
            let collected = rows.collect::<Result<Vec<_>, _>>().map_err(|error| {
                tracing::warn!("Holographic export row failed: {error}");
                MemoryProviderGovernanceError::failed(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                )
            })?;
            collected
        };
        transaction.commit().map_err(|error| {
            tracing::warn!("Holographic export snapshot commit failed: {error}");
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Export)
        })?;
        // The immutable rows are copied; do not hold the provider mutex while
        // an external transactional sink receives pages.
        drop(conn);
        let snapshot_bytes = serde_json::to_vec(&records).map_err(|error| {
            tracing::warn!("Holographic export snapshot serialization failed: {error}");
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Export)
        })?;
        let mut revision_bytes = b"holographic-v3".to_vec();
        revision_bytes.extend_from_slice(&snapshot_bytes);
        let snapshot = MemoryExportSnapshot::new(
            MemoryExportEvidenceDigest::sha256_bytes(&snapshot_bytes),
            MemoryExportEvidenceDigest::sha256_bytes(&revision_bytes),
            None,
        );
        if sink.begin_provider(snapshot.clone()).is_err() {
            sink.abort();
            return Err(MemoryProviderGovernanceError::failed(
                self.name(),
                MemoryGovernanceOperation::Export,
            ));
        }
        let mut exported_records = 0_u64;
        let mut page_count = 0_u64;
        for start in (0..records.len()).step_by(100) {
            let end = (start + 100).min(records.len());
            let cursor = (start != 0).then(|| {
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!(
                        "holographic-export:{}:{start}",
                        snapshot.snapshot_digest.as_str()
                    )
                    .as_bytes(),
                )
            });
            let next_cursor = (end < records.len()).then(|| {
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!(
                        "holographic-export:{}:{end}",
                        snapshot.snapshot_digest.as_str()
                    )
                    .as_bytes(),
                )
            });
            let record_ids = records[start..end]
                .iter()
                .map(|record| {
                    serde_json::to_vec(record)
                        .map(|bytes| MemoryExportEvidenceDigest::sha256_bytes(&bytes))
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    tracing::warn!("Holographic export record ID serialization failed: {error}");
                    MemoryProviderGovernanceError::failed(
                        self.name(),
                        MemoryGovernanceOperation::Export,
                    )
                })?;
            let page = MemoryExportPage::new(
                cursor,
                next_cursor,
                record_ids,
                records[start..end].to_vec(),
            )
            .map_err(|_| {
                MemoryProviderGovernanceError::failed(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                )
            })?;
            if sink.append_page(page).is_err() {
                sink.abort();
                return Err(MemoryProviderGovernanceError::failed(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                ));
            }
            exported_records = exported_records.saturating_add((end - start) as u64);
            page_count = page_count.saturating_add(1);
        }
        if sink
            .finish_provider(MemoryExportCompletion::exhausted(
                exported_records,
                page_count,
            ))
            .is_err()
        {
            sink.abort();
            return Err(MemoryProviderGovernanceError::failed(
                self.name(),
                MemoryGovernanceOperation::Export,
            ));
        }
        Ok(MemoryExportReceipt::exhausted(
            self.name(),
            filter.clone(),
            snapshot,
            exported_records,
            page_count,
        ))
    }

    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        let deleted_records = self.purge_subject(filter).map_err(|error| {
            tracing::warn!("Holographic forget failed: {error}");
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Forget)
        })?;
        let residual_records = self.subject_residue_count(filter).map_err(|error| {
            tracing::warn!("Holographic forget readback failed: {error}");
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Forget)
        })?;
        if residual_records != 0 {
            tracing::warn!("Holographic forget left subject residue");
            return Err(MemoryProviderGovernanceError::failed(
                self.name(),
                MemoryGovernanceOperation::Forget,
            ));
        }
        Ok(MemoryForgetReceipt::new(
            self.name(),
            filter.clone(),
            deleted_records,
        ))
    }

    fn retention_sweep(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        let deleted_records = self.apply_retention(filter, policy).map_err(|error| {
            tracing::warn!("Holographic retention sweep failed: {error}");
            MemoryProviderGovernanceError::failed(
                self.name(),
                MemoryGovernanceOperation::RetentionSweep,
            )
        })?;
        Ok(MemoryRetentionReceipt::new(
            self.name(),
            filter.clone(),
            deleted_records,
        ))
    }

    fn verify_subject_absent(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        let residual_records = self.subject_residue_count(filter).map_err(|error| {
            tracing::warn!("Holographic absence readback failed: {error}");
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Readback)
        })?;
        if residual_records == 0 {
            Ok(MemoryReadbackReceipt::absent(self.name(), filter.clone()))
        } else {
            Ok(MemoryReadbackReceipt::residual(
                self.name(),
                filter.clone(),
                residual_records,
            ))
        }
    }

    fn shutdown(&self) {
        self.release_connection();
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
    fn handle_fact_store(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        args: &Value,
    ) -> String {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(action) => action,
            None => return json!({"error": "Missing required argument: action"}).to_string(),
        };
        match action {
            "add" => {
                let content = match args.get("content").and_then(Value::as_str) {
                    Some(content) => content,
                    None => {
                        return json!({"error": "Missing required argument: content"}).to_string()
                    }
                };
                let category = args
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("general");
                let tags = args.get("tags").and_then(Value::as_str).unwrap_or("");
                match self.add_fact(subject, filter, content, category, tags) {
                    Ok(fact_id) => json!({"fact_id": fact_id, "status": "added"}).to_string(),
                    Err(error) => json!({"error": error}).to_string(),
                }
            }
            "search" => {
                let query = match args.get("query").and_then(Value::as_str) {
                    Some(query) => query,
                    None => {
                        return json!({"error": "Missing required argument: query"}).to_string()
                    }
                };
                let category = args.get("category").and_then(Value::as_str);
                let min_trust = args
                    .get("min_trust")
                    .and_then(Value::as_f64)
                    .unwrap_or(self.min_trust);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
                let results = self.search_facts(filter, query, category, min_trust, limit);
                json!({"results": results, "count": results.len()}).to_string()
            }
            "update" => self.update_fact(filter, args),
            "remove" => self.remove_fact(filter, args),
            "list" => self.list_facts(filter, args),
            _ => json!({"error": format!("Unknown action: {action}")}).to_string(),
        }
    }

    fn update_fact(&self, filter: &MemorySubjectStorageKey, args: &Value) -> String {
        let fact_id = match args.get("fact_id").and_then(Value::as_i64) {
            Some(fact_id) => fact_id,
            None => return json!({"error": "Missing required argument: fact_id"}).to_string(),
        };
        let Some(connection) = self.shared_connection() else {
            return json!({"error": "Not initialized"}).to_string();
        };
        let mut conn = connection.lock();
        let transaction = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(transaction) => transaction,
            Err(error) => return json!({"error": error.to_string()}).to_string(),
        };
        let mut assignments = vec!["updated_at = CURRENT_TIMESTAMP".to_string()];
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(content) = args.get("content").and_then(Value::as_str) {
            if content.trim().is_empty() {
                return json!({"error": "content must not be empty"}).to_string();
            }
            assignments.push(format!("content = ?{}", values.len() + 1));
            values.push(Box::new(content.to_string()));
        }
        if let Some(tags) = args.get("tags").and_then(Value::as_str) {
            assignments.push(format!("tags = ?{}", values.len() + 1));
            values.push(Box::new(tags.to_string()));
        }
        if let Some(category) = args.get("category").and_then(Value::as_str) {
            assignments.push(format!("category = ?{}", values.len() + 1));
            values.push(Box::new(category.to_string()));
        }
        if let Some(delta) = args.get("trust_delta").and_then(Value::as_f64) {
            let old_trust = match transaction.query_row(
                "SELECT trust_score FROM facts WHERE fact_id = ?1 AND subject_key = ?2",
                params![fact_id, filter.as_str()],
                |row| row.get::<_, f64>(0),
            ) {
                Ok(trust) => trust,
                Err(_) => {
                    return json!({"error": format!("fact_id {fact_id} not found")}).to_string()
                }
            };
            assignments.push(format!("trust_score = ?{}", values.len() + 1));
            values.push(Box::new(clamp_trust(old_trust + delta)));
        }
        values.push(Box::new(fact_id));
        values.push(Box::new(filter.as_str().to_string()));
        let sql = format!(
            "UPDATE facts SET {} WHERE fact_id = ?{} AND subject_key = ?{}",
            assignments.join(", "),
            values.len() - 1,
            values.len()
        );
        let references = values
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<&dyn rusqlite::types::ToSql>>();
        match transaction.execute(&sql, references.as_slice()) {
            Ok(changed) => match transaction.commit() {
                Ok(()) => json!({"updated": changed > 0}).to_string(),
                Err(error) => json!({"error": error.to_string()}).to_string(),
            },
            Err(error) => json!({"error": error.to_string()}).to_string(),
        }
    }

    fn remove_fact(&self, filter: &MemorySubjectStorageKey, args: &Value) -> String {
        let fact_id = match args.get("fact_id").and_then(Value::as_i64) {
            Some(fact_id) => fact_id,
            None => return json!({"error": "Missing required argument: fact_id"}).to_string(),
        };
        let Some(connection) = self.shared_connection() else {
            return json!({"error": "Not initialized"}).to_string();
        };
        let mut conn = connection.lock();
        let transaction = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
            Ok(transaction) => transaction,
            Err(error) => return json!({"error": error.to_string()}).to_string(),
        };
        if let Err(error) = transaction.execute(
            "DELETE FROM fact_entities WHERE subject_key = ?1 AND fact_id = ?2",
            params![filter.as_str(), fact_id],
        ) {
            return json!({"error": error.to_string()}).to_string();
        }
        match transaction.execute(
            "DELETE FROM facts WHERE subject_key = ?1 AND fact_id = ?2",
            params![filter.as_str(), fact_id],
        ) {
            Ok(changed) => match transaction.commit() {
                Ok(()) => json!({"removed": changed > 0}).to_string(),
                Err(error) => json!({"error": error.to_string()}).to_string(),
            },
            Err(error) => json!({"error": error.to_string()}).to_string(),
        }
    }

    fn list_facts(&self, filter: &MemorySubjectStorageKey, args: &Value) -> String {
        let Some(connection) = self.shared_connection() else {
            return json!({"error": "Not initialized"}).to_string();
        };
        let conn = connection.lock();
        let category = args.get("category").and_then(Value::as_str);
        let min_trust = args.get("min_trust").and_then(Value::as_f64).unwrap_or(0.0);
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as i64;
        let sql = if category.is_some() {
            "
            SELECT fact_id, content, category, tags, trust_score, retrieval_count,
                   helpful_count, created_at, updated_at
            FROM facts
            WHERE subject_key = ?1 AND trust_score >= ?2 AND category = ?3
            ORDER BY trust_score DESC
            LIMIT ?4
            "
        } else {
            "
            SELECT fact_id, content, category, tags, trust_score, retrieval_count,
                   helpful_count, created_at, updated_at
            FROM facts
            WHERE subject_key = ?1 AND trust_score >= ?2
            ORDER BY trust_score DESC
            LIMIT ?3
            "
        };
        let mut statement = match conn.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return json!({"error": error.to_string()}).to_string(),
        };
        let rows = if let Some(category) = category {
            statement.query_map(
                params![filter.as_str(), min_trust, category, limit],
                row_to_fact,
            )
        } else {
            statement.query_map(params![filter.as_str(), min_trust, limit], row_to_fact)
        };
        match rows {
            Ok(rows) => {
                let facts = rows.flatten().collect::<Vec<_>>();
                json!({"facts": facts, "count": facts.len()}).to_string()
            }
            Err(error) => json!({"error": error.to_string()}).to_string(),
        }
    }

    fn handle_fact_feedback(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        args: &Value,
    ) -> String {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(action) => action,
            None => return json!({"error": "Missing required argument: action"}).to_string(),
        };
        let fact_id = match args.get("fact_id").and_then(Value::as_i64) {
            Some(fact_id) => fact_id,
            None => return json!({"error": "Missing required argument: fact_id"}).to_string(),
        };
        match self.record_feedback(subject, filter, fact_id, action == "helpful") {
            Ok(result) => result.to_string(),
            Err(error) => json!({"error": error}).to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[derive(Default)]
    struct TestExportSink {
        snapshot: Option<hermes_core::MemoryExportSnapshot>,
        pages: Vec<hermes_core::MemoryExportPage>,
        completion: Option<hermes_core::MemoryExportCompletion>,
        aborted: bool,
    }

    impl hermes_core::MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            snapshot: hermes_core::MemoryExportSnapshot,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.snapshot = Some(snapshot);
            Ok(())
        }

        fn append_page(
            &mut self,
            page: hermes_core::MemoryExportPage,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            completion: hermes_core::MemoryExportCompletion,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            self.completion = Some(completion);
            Ok(())
        }

        fn abort(&mut self) {
            self.aborted = true;
        }

        fn commit(
            self: Box<Self>,
        ) -> Result<hermes_core::MemoryExportArtifact, hermes_core::MemoryExportSinkError> {
            Err(hermes_core::MemoryExportSinkError::Incomplete)
        }
    }
    struct MutatingExportSink {
        plugin: Arc<HolographicMemoryPlugin>,
        subject: MemorySubject,
        filter: MemorySubjectStorageKey,
        pages: Vec<hermes_core::MemoryExportPage>,
        mutated: bool,
    }

    impl hermes_core::MemoryExportSink for MutatingExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: hermes_core::MemoryExportSnapshot,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn append_page(
            &mut self,
            page: hermes_core::MemoryExportPage,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            if !self.mutated {
                self.plugin
                    .add_fact(
                        &self.subject,
                        &self.filter,
                        "written after export snapshot",
                        "test",
                        "",
                    )
                    .map_err(|_| hermes_core::MemoryExportSinkError::Rejected)?;
                self.mutated = true;
            }
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            _completion: hermes_core::MemoryExportCompletion,
        ) -> Result<(), hermes_core::MemoryExportSinkError> {
            Ok(())
        }

        fn abort(&mut self) {}

        fn commit(
            self: Box<Self>,
        ) -> Result<hermes_core::MemoryExportArtifact, hermes_core::MemoryExportSinkError> {
            Err(hermes_core::MemoryExportSinkError::Incomplete)
        }
    }

    fn subject(label: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::local_owner(hermes_core::MemorySurface::Local, label)
            .expect("explicit local subject");
        let derivation = hermes_core::MemorySubjectDerivationKey::from_bytes([29_u8; 32]);
        let key = subject.storage_key(&derivation);
        (subject, key)
    }

    fn actor(subject: &MemorySubject) -> MemoryAuditActor {
        hermes_core::MemorySubjectDerivationKey::from_bytes([31_u8; 32]).actor_for_subject(subject)
    }

    fn initialize(
        plugin: &HolographicMemoryPlugin,
        subject: &MemorySubject,
        key: &MemorySubjectStorageKey,
        home: &std::path::Path,
    ) {
        MemoryProviderPlugin::initialize(plugin, subject, key, home.to_str().expect("home"))
            .expect("initialize scoped database");
    }

    #[test]
    fn facts_and_searches_are_subject_partitioned() {
        let home = tempfile::tempdir().expect("tempdir");
        let (alice, alice_key) = subject("alice-session");
        let (bob, bob_key) = subject("bob-session");
        let plugin = HolographicMemoryPlugin::new();
        initialize(&plugin, &alice, &alice_key, home.path());

        plugin
            .add_fact(
                &alice,
                &alice_key,
                "Alice uses the blue theme",
                "preference",
                "theme",
            )
            .expect("alice fact");
        plugin
            .add_fact(
                &bob,
                &bob_key,
                "Bob uses the red theme",
                "preference",
                "theme",
            )
            .expect("bob fact");

        let alice_results = plugin.search_facts(&alice_key, "theme", None, 0.0, 10);
        let bob_results = plugin.search_facts(&bob_key, "theme", None, 0.0, 10);
        assert_eq!(plugin.fact_count(&alice_key), 1);
        assert_eq!(plugin.fact_count(&bob_key), 1);
        assert_eq!(alice_results.len(), 1);
        assert_eq!(bob_results.len(), 1);
        assert_eq!(alice_results[0]["content"], "Alice uses the blue theme");
        assert_eq!(bob_results[0]["content"], "Bob uses the red theme");
    }

    #[test]
    fn legacy_unscoped_rows_are_quarantined_at_schema_v3() {
        let home = tempfile::tempdir().expect("tempdir");
        let db = home.path().join("memory_store.db");
        let legacy = Connection::open(&db).expect("legacy database");
        legacy
            .execute_batch(
                "
                CREATE TABLE facts (
                    fact_id INTEGER PRIMARY KEY,
                    content TEXT NOT NULL,
                    category TEXT,
                    tags TEXT,
                    trust_score REAL
                );
                INSERT INTO facts (fact_id, content, category, tags, trust_score)
                VALUES (1, 'unscoped secret', 'general', '', 0.5);
                ",
            )
            .expect("legacy data");
        drop(legacy);

        let (subject, key) = subject("migration-session");
        let plugin = HolographicMemoryPlugin::new();
        initialize(&plugin, &subject, &key, home.path());
        assert_eq!(plugin.fact_count(&key), 0);
        let report = *plugin.migration_report.lock();
        assert_eq!(report.schema_version, HOLOGRAPHIC_SCHEMA_VERSION);
        assert_eq!(report.quarantined_facts, 1);
        plugin.with_conn(|conn| {
            assert!(sqlite_table_exists(conn, "facts_legacy_quarantine_v1").expect("quarantine"));
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM facts_legacy_quarantine_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("legacy count"),
                1
            );
        });
    }

    #[test]
    fn concurrent_scoped_writers_preserve_every_fact() {
        let home = tempfile::tempdir().expect("tempdir");
        let (subject, key) = subject("concurrent-session");
        let first = Arc::new(HolographicMemoryPlugin::new());
        let second = Arc::new(HolographicMemoryPlugin::new());
        initialize(&first, &subject, &key, home.path());
        initialize(&second, &subject, &key, home.path());

        let mut workers = Vec::new();
        for worker in 0..2 {
            let plugin = if worker == 0 {
                Arc::clone(&first)
            } else {
                Arc::clone(&second)
            };
            let subject = subject.clone();
            let key = key.clone();
            workers.push(std::thread::spawn(move || {
                for index in 0..12 {
                    plugin
                        .add_fact(
                            &subject,
                            &key,
                            &format!("concurrent fact {worker}-{index}"),
                            "test",
                            "",
                        )
                        .expect("concurrent add");
                }
            }));
        }
        for worker in workers {
            worker.join().expect("worker");
        }
        assert_eq!(first.fact_count(&key), 24);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_database_aliases_share_connection_and_survive_peer_shutdown() {
        let real_home = tempfile::tempdir().expect("real home");
        let alias_root = tempfile::tempdir().expect("alias root");
        let alias_home = alias_root.path().join("database-home");
        std::os::unix::fs::symlink(real_home.path(), &alias_home).expect("directory symlink");
        let (subject, key) = subject("symlink-session");
        let first = HolographicMemoryPlugin::new();
        let second = HolographicMemoryPlugin::new();
        initialize(&first, &subject, &key, real_home.path());
        initialize(&second, &subject, &key, &alias_home);

        let first_connection = first.shared_connection().expect("first connection");
        let second_connection = second.shared_connection().expect("second connection");
        assert!(Arc::ptr_eq(&first_connection, &second_connection));
        drop(first_connection);
        drop(second_connection);

        first
            .add_fact(&subject, &key, "first alias fact", "test", "")
            .expect("first alias write");
        MemoryProviderPlugin::shutdown(&first);
        second
            .add_fact(&subject, &key, "second alias fact", "test", "")
            .expect("surviving alias write");
        assert_eq!(second.fact_count(&key), 2);
    }

    #[test]
    fn forget_clears_facts_entities_and_fts_then_readback_misses() {
        let home = tempfile::tempdir().expect("tempdir");
        let (subject, key) = subject("forget-session");
        let plugin = HolographicMemoryPlugin::new();
        initialize(&plugin, &subject, &key, home.path());
        plugin
            .add_fact(
                &subject,
                &key,
                "Ada maintains the deployment ledger",
                "project",
                "Ada",
            )
            .expect("fact");
        let actor = actor(&subject);

        let receipt =
            MemoryProviderPlugin::forget_subject(&plugin, &subject, &key, &actor).expect("forget");
        assert!(receipt.deleted_records >= 1);
        assert!(plugin
            .search_facts(&key, "deployment", None, 0.0, 10)
            .is_empty());
        assert!(
            MemoryProviderPlugin::verify_subject_absent(&plugin, &subject, &key)
                .expect("readback")
                .is_absent()
        );
        plugin.with_conn(|conn| {
            for table in ["facts", "entities", "fact_entities"] {
                let count = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE subject_key = ?1"),
                        params![key.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("subject residue count");
                assert_eq!(count, 0, "{table} residue");
            }
            if sqlite_table_exists(conn, "facts_fts").expect("fts table") {
                let count = conn
                    .query_row(
                        "SELECT COUNT(*) FROM facts_fts WHERE subject_key = ?1",
                        params![key.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("fts residue count");
                assert_eq!(count, 0);
            }
        });
    }

    #[test]
    fn export_and_retention_only_affect_the_bound_subject() {
        let home = tempfile::tempdir().expect("tempdir");
        let (alice, alice_key) = subject("export-alice");
        let (bob, bob_key) = subject("export-bob");
        let plugin = HolographicMemoryPlugin::new();
        initialize(&plugin, &alice, &alice_key, home.path());
        plugin
            .add_fact(&alice, &alice_key, "alice old record", "test", "")
            .expect("old");
        plugin
            .add_fact(&alice, &alice_key, "alice new record", "test", "")
            .expect("new");
        plugin
            .add_fact(&bob, &bob_key, "bob retained record", "test", "")
            .expect("bob");
        plugin.with_conn(|conn| {
            conn.execute(
                "UPDATE facts SET created_at = datetime('now', '-30 days') WHERE subject_key = ?1 AND content = 'alice old record'",
                params![alice_key.as_str()],
            )
            .expect("age first record");
        });
        let alice_actor = actor(&alice);
        let mut sink = TestExportSink::default();
        let exported = MemoryProviderPlugin::export_subject(
            &plugin,
            &alice,
            &alice_key,
            &alice_actor,
            &mut sink,
        )
        .expect("export");
        assert_eq!(exported.exported_records, 2);
        assert_eq!(sink.pages.len(), 1);
        assert!(sink.completion.expect("completion").exhausted);

        let policy = MemoryRetentionPolicy::new(Some(7), None).expect("retention policy");
        let retained = MemoryProviderPlugin::retention_sweep(
            &plugin,
            &alice,
            &alice_key,
            &policy,
            &alice_actor,
        )
        .expect("retention");
        assert_eq!(retained.deleted_records, 1);
        assert_eq!(plugin.fact_count(&alice_key), 1);
        assert_eq!(plugin.fact_count(&bob_key), 1);
        assert_eq!(
            plugin.search_facts(&bob_key, "bob retained", None, 0.0, 10)[0]["content"],
            "bob retained record"
        );
    }

    #[test]
    fn export_uses_exhaustive_pages_and_a_nonlooping_cursor_chain() {
        let home = tempfile::tempdir().expect("tempdir");
        let (subject, filter) = subject("paged-export");
        let plugin = HolographicMemoryPlugin::new();
        initialize(&plugin, &subject, &filter, home.path());
        for index in 0..101 {
            plugin
                .add_fact(
                    &subject,
                    &filter,
                    &format!("paged fact {index}"),
                    "test",
                    "",
                )
                .expect("fact");
        }
        let mut sink = TestExportSink::default();
        let receipt = MemoryProviderPlugin::export_subject(
            &plugin,
            &subject,
            &filter,
            &actor(&subject),
            &mut sink,
        )
        .expect("paged export");
        assert_eq!(receipt.exported_records, 101);
        assert_eq!(receipt.page_count, 2);
        assert_eq!(sink.pages.len(), 2);
        assert_eq!(sink.pages[0].records.len(), 100);
        assert_eq!(sink.pages[1].records.len(), 1);
        assert_eq!(sink.pages[0].cursor, None);
        assert_eq!(sink.pages[0].next_cursor, sink.pages[1].cursor);
        assert_eq!(sink.pages[1].next_cursor, None);
        let record_ids = sink
            .pages
            .iter()
            .flat_map(|page| page.record_ids.iter().map(|id| id.as_str()))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(record_ids.len(), 101);
    }

    #[test]
    fn export_snapshot_is_stable_when_a_sink_causes_a_later_write() {
        let home = tempfile::tempdir().expect("tempdir");
        let (subject, filter) = subject("snapshot-mutation");
        let plugin = Arc::new(HolographicMemoryPlugin::new());
        initialize(plugin.as_ref(), &subject, &filter, home.path());
        for index in 0..101 {
            plugin
                .add_fact(
                    &subject,
                    &filter,
                    &format!("snapshot fact {index}"),
                    "test",
                    "",
                )
                .expect("fact");
        }
        let mut sink = MutatingExportSink {
            plugin: Arc::clone(&plugin),
            subject: subject.clone(),
            filter: filter.clone(),
            pages: Vec::new(),
            mutated: false,
        };
        let receipt = MemoryProviderPlugin::export_subject(
            plugin.as_ref(),
            &subject,
            &filter,
            &actor(&subject),
            &mut sink,
        )
        .expect("snapshot export");
        assert_eq!(receipt.exported_records, 101);
        assert!(sink.mutated);
        assert!(sink
            .pages
            .iter()
            .flat_map(|page| page.records.iter())
            .all(|record| { record["content"] != "written after export snapshot" }));
        assert_eq!(plugin.fact_count(&filter), 102);
    }
}
