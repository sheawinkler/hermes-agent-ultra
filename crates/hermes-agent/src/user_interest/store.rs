//! SQLite-backed local user interest (POI) store.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use hermes_config::InterestConfig;
use hermes_insights::sanitize::is_persistable_local_poi;
use rusqlite::{params, Connection};

use super::types::{SignalSource, TopicStatus};

const ENTRY_DELIMITER: &str = "\n§\n";
const SCHEMA_VERSION: i32 = 2;

/// A single interest topic row.
#[derive(Debug, Clone)]
pub struct InterestTopic {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub weight: f64,
    pub last_seen_at: DateTime<Utc>,
    pub evidence_count: u32,
    pub tags: Vec<String>,
    pub status: TopicStatus,
    pub source: SignalSource,
    pub confidence: f64,
    pub pinned: bool,
}

/// Incremental update from rules or LLM extraction.
#[derive(Debug, Clone)]
pub struct InterestSignal {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub weight_delta: f64,
    pub tags: Vec<String>,
    pub source: SignalSource,
    pub confidence: f64,
}

impl InterestSignal {
    pub fn source(&self) -> SignalSource {
        self.source
    }

    pub fn new(
        id: String,
        label: String,
        summary: String,
        weight_delta: f64,
        tags: Vec<String>,
        source: SignalSource,
    ) -> Self {
        Self {
            id,
            label,
            summary,
            weight_delta,
            tags,
            source,
            confidence: source.default_confidence(),
        }
    }
}

/// Local interest database.
pub struct InterestStore {
    conn: Mutex<Connection>,
    config: InterestConfig,
}

impl InterestStore {
    pub fn open(db_path: &Path, config: InterestConfig) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        let store = Self {
            conn: Mutex::new(conn),
            config,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS topics (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                summary TEXT NOT NULL DEFAULT '',
                weight REAL NOT NULL DEFAULT 0.1,
                last_seen_at TEXT NOT NULL,
                evidence_count INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_topics_weight ON topics(weight DESC);
            CREATE INDEX IF NOT EXISTS idx_topics_last_seen ON topics(last_seen_at DESC);",
        )
        .map_err(|e| e.to_string())?;

        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);
        if version < SCHEMA_VERSION {
            Self::migrate_v2_columns(&conn)?;
            conn.execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), [])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn migrate_v2_columns(conn: &Connection) -> Result<(), String> {
        let mut cols = HashSet::new();
        let mut stmt = conn
            .prepare("PRAGMA table_info(topics)")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?;
        for name in rows.flatten() {
            cols.insert(name);
        }
        if !cols.contains("status") {
            conn.execute(
                "ALTER TABLE topics ADD COLUMN status TEXT NOT NULL DEFAULT 'active'",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        if !cols.contains("source") {
            conn.execute(
                "ALTER TABLE topics ADD COLUMN source TEXT NOT NULL DEFAULT 'rules'",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        if !cols.contains("confidence") {
            conn.execute(
                "ALTER TABLE topics ADD COLUMN confidence REAL NOT NULL DEFAULT 0.5",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        if !cols.contains("pinned") {
            conn.execute(
                "ALTER TABLE topics ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn config(&self) -> &InterestConfig {
        &self.config
    }

    pub fn apply_decay(&self) -> Result<(), String> {
        let half_life = self.config.decay_half_life_days.max(1.0);
        let factor = 0.5_f64.powf(1.0 / half_life);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE topics SET weight = MAX(0.05, weight * ?1), updated_at = ?2
             WHERE status != 'rejected'",
            params![factor, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Apply extracted signals through the production compare → update pipeline.
    pub fn ingest_signals(&self, signals: &[InterestSignal]) -> Result<(), String> {
        super::pipeline::apply_signal_batch(self, &self.config, signals.to_vec())?;
        Ok(())
    }

    pub fn list_topics_for_pipeline(&self) -> Result<Vec<InterestTopic>, String> {
        self.query_topics(
            "SELECT id, label, summary, weight, last_seen_at, evidence_count, tags,
                    status, source, confidence, pinned
             FROM topics WHERE status != 'rejected'
             ORDER BY weight DESC, last_seen_at DESC",
            None,
        )
    }

    pub fn insert_topic(&self, signal: &InterestSignal, status: TopicStatus) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let tags_json =
            serde_json::to_string(&signal.tags).unwrap_or_else(|_| "[]".to_string());
        let weight = signal.weight_delta.clamp(0.08, 0.5);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO topics (id, label, summary, weight, last_seen_at, evidence_count, tags,
             created_at, updated_at, status, source, confidence, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?5, ?5, ?7, ?8, ?9, 0)",
            params![
                signal.id,
                signal.label,
                signal.summary,
                weight,
                now,
                tags_json,
                status.as_str(),
                signal.source.as_str(),
                signal.confidence,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Reinforce an existing row; returns true if status was promoted to `active`.
    pub fn reinforce_topic(
        &self,
        topic_id: &str,
        signal: &InterestSignal,
        promote_min_evidence: u32,
        promote_min_confidence: f64,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let existing: Option<(f64, u32, String, String, String, f64, i32)> = conn
            .query_row(
                "SELECT weight, evidence_count, summary, label, status, confidence, pinned
                 FROM topics WHERE id = ?1",
                params![topic_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .ok();
        let Some((weight, count, old_summary, old_label, status_raw, old_conf, pinned)) =
            existing
        else {
            return Ok(false);
        };
        let new_weight = (weight + signal.weight_delta).min(1.0);
        let new_count = count + 1;
        let summary = if signal.summary.len() > old_summary.len() {
            signal.summary.clone()
        } else {
            old_summary
        };
        let label = merge_topic_label(&old_label, &signal.label);
        let new_conf = old_conf.max(signal.confidence);
        let mut status = TopicStatus::parse(&status_raw);
        let mut promoted = false;
        if pinned == 0 && status == TopicStatus::Candidate {
            if new_count >= promote_min_evidence && new_conf >= promote_min_confidence {
                status = TopicStatus::Active;
                promoted = true;
            }
        }
        let tags_json =
            serde_json::to_string(&signal.tags).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE topics SET label = ?1, summary = ?2, weight = ?3, last_seen_at = ?4,
             evidence_count = ?5, tags = ?6, updated_at = ?4, status = ?7, confidence = ?8,
             source = ?9 WHERE id = ?10",
            params![
                label,
                summary,
                new_weight,
                now,
                new_count,
                tags_json,
                status.as_str(),
                new_conf,
                signal.source.as_str(),
                topic_id,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(promoted)
    }

    pub fn enforce_max_topics(&self) -> Result<(), String> {
        let max = self.config.max_topics.max(5) as i64;
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM topics WHERE status != 'rejected'", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        if count <= max {
            return Ok(());
        }
        let excess = count - max;
        conn.execute(
            "DELETE FROM topics WHERE id IN (
                SELECT id FROM topics WHERE status != 'rejected'
                ORDER BY
                  CASE status WHEN 'candidate' THEN 0 WHEN 'active' THEN 1 ELSE 2 END,
                  pinned ASC,
                  weight ASC,
                  last_seen_at ASC
                LIMIT ?1
            )",
            params![excess],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn top_topics(&self, limit: usize) -> Result<Vec<InterestTopic>, String> {
        self.query_topics(
            "SELECT id, label, summary, weight, last_seen_at, evidence_count, tags,
                    status, source, confidence, pinned
             FROM topics
             WHERE status = 'active' OR pinned = 1
             ORDER BY pinned DESC, weight DESC, last_seen_at DESC
             LIMIT ?1",
            Some(limit as i64),
        )
    }

    pub fn score_for_query(&self, query: &str, limit: usize) -> Result<Vec<InterestTopic>, String> {
        let all = self.top_topics(self.config.max_topics as usize)?;
        let q = query.to_ascii_lowercase();
        let q_tokens: Vec<&str> = q
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 3)
            .collect();
        if q_tokens.is_empty() {
            return Ok(all.into_iter().take(limit).collect());
        }
        let mut scored: Vec<(f64, InterestTopic)> = all
            .into_iter()
            .map(|topic| {
                let hay = format!(
                    "{} {} {}",
                    topic.label.to_ascii_lowercase(),
                    topic.summary.to_ascii_lowercase(),
                    topic.tags.join(" ").to_ascii_lowercase()
                );
                let mut overlap = 0usize;
                for tok in &q_tokens {
                    if hay.contains(tok) {
                        overlap += 1;
                    }
                }
                let lexical = overlap as f64 / q_tokens.len() as f64;
                let score = topic.weight * (0.35 + 0.65 * lexical);
                (score, topic)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().take(limit).map(|(_, t)| t).collect())
    }

    pub fn render_snapshot_block(&self) -> Option<String> {
        let top_k = self.config.snapshot_top_k.max(1) as usize;
        let budget = self.config.char_budget_snapshot.max(120);
        let topics = self.top_topics(top_k).ok()?;
        self.render_block(
            "USER INTERESTS (topics this user often works on)",
            &topics,
            budget,
        )
    }

    pub fn render_prefetch_block(&self, query: &str) -> Option<String> {
        let top_k = self.config.prefetch_top_k.max(1) as usize;
        let budget = self.config.char_budget_prefetch.max(80);
        let topics = self.score_for_query(query, top_k).ok()?;
        if topics.is_empty() {
            return None;
        }
        self.render_block("Relevant user interests for this turn", &topics, budget)
    }

    fn render_block(
        &self,
        label: &str,
        topics: &[InterestTopic],
        char_budget: usize,
    ) -> Option<String> {
        if topics.is_empty() {
            return None;
        }
        let mut entries = Vec::new();
        let mut used = 0usize;
        for topic in topics {
            let line = if topic.summary.trim().is_empty() {
                topic.label.clone()
            } else {
                format!("{} — {}", topic.label, topic.summary)
            };
            let line_len = line.chars().count() + ENTRY_DELIMITER.chars().count();
            if used + line_len > char_budget && !entries.is_empty() {
                break;
            }
            entries.push(line);
            used += line_len;
        }
        if entries.is_empty() {
            return None;
        }
        let content = entries.join(ENTRY_DELIMITER);
        let current = content.chars().count();
        let pct = ((current * 100) / char_budget).min(100);
        Some(format!(
            "══════════════════════════════════════════════\n\
             {label} [{pct}% — {current}/{char_budget} chars]\n\
             ══════════════════════════════════════════════\n\
             {content}"
        ))
    }

    pub fn list_for_cli(&self, include_candidates: bool) -> Result<Vec<InterestTopic>, String> {
        if include_candidates {
            self.query_topics(
                "SELECT id, label, summary, weight, last_seen_at, evidence_count, tags,
                        status, source, confidence, pinned
                 FROM topics WHERE status != 'rejected'
                 ORDER BY weight DESC, last_seen_at DESC",
                None,
            )
        } else {
            self.top_topics(self.config.max_topics as usize)
        }
    }

    pub fn set_topic_status(&self, topic_id: &str, status: TopicStatus) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let updated = conn
            .execute(
                "UPDATE topics SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.as_str(), Utc::now().to_rfc3339(), topic_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(updated > 0)
    }

    pub fn pin_topic(&self, topic_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let updated = conn
            .execute(
                "UPDATE topics SET pinned = 1, status = 'active', updated_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), topic_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(updated > 0)
    }

    pub fn top_labels_for_llm(&self, limit: usize) -> Result<Vec<String>, String> {
        Ok(self
            .top_topics(limit)?
            .into_iter()
            .map(|t| t.label)
            .collect())
    }

    /// Remove rows that fail current POI quality filters (legacy noise).
    pub fn prune_rejected_topics(&self) -> Result<usize, String> {
        let topics = self.list_for_cli(true)?;
        let ids: Vec<String> = topics
            .iter()
            .filter(|t| {
                super::extract::is_rejected_poi_topic(&t.id, &t.label)
                    || !is_persistable_local_poi(&t.id, &t.label)
                    || t.id.starts_with("keyword:")
                    || t.id.starts_with("path:")
            })
            .map(|t| t.id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        for id in &ids {
            conn.execute("DELETE FROM topics WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
        }
        Ok(ids.len())
    }

    fn query_topics(&self, sql: &str, limit: Option<i64>) -> Result<Vec<InterestTopic>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let map_row = |row: &rusqlite::Row<'_>| {
            let status_raw: String = row.get(7)?;
            let source_raw: String = row.get(8)?;
            Ok(InterestTopic {
                id: row.get(0)?,
                label: row.get(1)?,
                summary: row.get(2)?,
                weight: row.get(3)?,
                last_seen_at: parse_rfc3339(row.get::<_, String>(4)?),
                evidence_count: row.get::<_, i64>(5)? as u32,
                tags: parse_tags(row.get::<_, String>(6)?),
                status: TopicStatus::parse(&status_raw),
                source: parse_source(&source_raw),
                confidence: row.get(9)?,
                pinned: row.get::<_, i64>(10)? != 0,
            })
        };
        let rows = if let Some(lim) = limit {
            stmt.query_map(params![lim], map_row)
        } else {
            stmt.query_map([], map_row)
        }
        .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

fn parse_source(raw: &str) -> SignalSource {
    match raw.trim().to_ascii_lowercase().as_str() {
        "declared" => SignalSource::Declared,
        "lang" => SignalSource::Lang,
        "tech" => SignalSource::Tech,
        "path" => SignalSource::Path,
        "keyword" => SignalSource::Keyword,
        "llm" => SignalSource::Llm,
        _ => SignalSource::Rules,
    }
}

/// Prefer explicit interest labels; avoid replacing a specific label with a generic one.
fn merge_topic_label(old: &str, new: &str) -> String {
    let old = old.trim();
    let new = new.trim();
    if new.is_empty() {
        return old.to_string();
    }
    if old.is_empty() {
        return new.to_string();
    }
    if old == new {
        return old.to_string();
    }
    let old_declared = old.starts_with("兴趣:");
    let new_declared = new.starts_with("兴趣:");
    if new_declared && !old_declared {
        return new.to_string();
    }
    if old_declared && !new_declared {
        return old.to_string();
    }
    if new.chars().count() > old.chars().count() {
        new.to_string()
    } else {
        old.to_string()
    }
}

fn parse_rfc3339(raw: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_tags(raw: String) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
}

/// Load frozen interest snapshot for system prompt assembly.
pub fn load_interest_snapshot(
    hermes_home: Option<&str>,
    config: &InterestConfig,
) -> Option<String> {
    if !config.enabled {
        return None;
    }
    let home = hermes_home
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var("HERMES_HOME").ok().map(std::path::PathBuf::from))
        .or_else(|| Some(hermes_config::hermes_home()))
        .unwrap_or_else(hermes_config::hermes_home);
    let db_path = home.join("interest.db");
    let store = InterestStore::open(&db_path, config.clone()).ok()?;
    store.render_snapshot_block()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_interest::extract::extract_signals_from_text;
    use crate::user_interest::ExtractOptions;
    use tempfile::TempDir;

    #[test]
    fn ingest_two_distinct_chinese_interests() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("interest.db");
        let config = InterestConfig::default();
        let store = InterestStore::open(&db, config).unwrap();
        let mut batch = extract_signals_from_text(
            "我的兴趣点是打篮球",
            1.0,
            ExtractOptions::default(),
        );
        batch.extend(extract_signals_from_text(
            "我的兴趣点还有吃鱼",
            1.0,
            ExtractOptions::default(),
        ));
        store.ingest_signals(&batch).unwrap();
        let topics = store.list_for_cli(true).unwrap();
        let interest_rows: Vec<_> = topics
            .iter()
            .filter(|t| t.id.starts_with("interest:"))
            .collect();
        assert!(interest_rows.len() >= 2);
    }

    #[test]
    fn ingest_and_snapshot() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("interest.db");
        let config = InterestConfig::default();
        let store = InterestStore::open(&db, config).unwrap();
        store
            .ingest_signals(&[InterestSignal::new(
                "tech:rust".to_string(),
                "topic: rust".to_string(),
                "User works on Rust agent runtime".to_string(),
                0.35,
                vec!["rust".to_string()],
                SignalSource::Tech,
            )])
            .unwrap();
        let block = store.render_snapshot_block();
        assert!(block.is_some());
        assert!(block.unwrap().contains("rust"));
    }
}
