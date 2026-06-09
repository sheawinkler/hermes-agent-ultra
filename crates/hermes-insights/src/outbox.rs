//! Local SQLite outbox for reliable contribution delivery.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::types::ContributionEnvelope;
use crate::types::{work_package_id, ContributionType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    Sent,
    Failed,
    Rejected,
}

impl OutboxStatus {
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            "rejected" => Self::Rejected,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub id: String,
    pub kind: String,
    pub envelope: ContributionEnvelope,
    pub status: OutboxStatus,
    pub attempts: u32,
}

pub struct ContributionOutbox {
    conn: Mutex<Connection>,
}

impl ContributionOutbox {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        let outbox = Self {
            conn: Mutex::new(conn),
        };
        outbox.migrate()?;
        Ok(outbox)
    }

    fn migrate(&self) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS contributions (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                attempts INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                sent_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_contributions_status ON contributions(status);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_contributions_hash ON contributions(content_hash);",
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn enqueue(&self, envelope: ContributionEnvelope) -> Result<bool, String> {
        if envelope.kind == ContributionType::DomainWorkPackage.as_str() {
            if let Some(wid) = crate::types::work_package_id(&envelope.payload) {
                self.drop_pending_work_package(&wid)?;
            }
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let payload_json =
            serde_json::to_string(&envelope).map_err(|e| format!("serialize: {e}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn
            .execute(
                "INSERT OR IGNORE INTO contributions
                 (id, type, payload_json, content_hash, status, attempts, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    envelope.kind,
                    payload_json,
                    envelope.content_hash,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(rows > 0)
    }

    /// Delete pending/failed work packages with the same `work_id` before re-enqueue.
    fn drop_pending_work_package(&self, work_id: &str) -> Result<(), String> {
        let pending = self.list_pending(512)?;
        let ids: Vec<String> = pending
            .into_iter()
            .filter(|e| {
                e.kind == ContributionType::DomainWorkPackage.as_str()
                    && work_package_id(&e.envelope.payload).as_deref() == Some(work_id)
            })
            .map(|e| e.id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        for id in ids {
            conn.execute("DELETE FROM contributions WHERE id = ?1", rusqlite::params![id])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn list_pending(&self, limit: usize) -> Result<Vec<OutboxEntry>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, type, payload_json, content_hash, status, attempts
                 FROM contributions WHERE status IN ('pending', 'failed')
                 ORDER BY created_at ASC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let payload_json: String = row.get(2)?;
                let envelope: ContributionEnvelope =
                    serde_json::from_str(&payload_json).map_err(|e| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                            e.to_string(),
                        )))
                    })?;
                Ok(OutboxEntry {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    envelope,
                    status: OutboxStatus::parse(&row.get::<_, String>(4)?),
                    attempts: row.get::<_, i64>(5)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn update_envelope(&self, id: &str, envelope: ContributionEnvelope) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let payload_json =
            serde_json::to_string(&envelope).map_err(|e| format!("serialize: {e}"))?;
        conn.execute(
            "UPDATE contributions SET type = ?1, payload_json = ?2, content_hash = ?3 WHERE id = ?4",
            params![envelope.kind, payload_json, envelope.content_hash, id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn mark_sent(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        for id in ids {
            conn.execute(
                "UPDATE contributions SET status = 'sent', sent_at = ?1 WHERE id = ?2",
                params![now, id],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn mark_failed(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        for id in ids {
            conn.execute(
                "UPDATE contributions SET status = 'failed', attempts = attempts + 1 WHERE id = ?1",
                params![id],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Move `sent` / `failed` rows back to `pending` for local re-upload testing.
    pub fn reset_sent_to_pending(&self) -> Result<u32, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let n = conn
            .execute(
                "UPDATE contributions SET status = 'pending', sent_at = NULL, attempts = 0
                 WHERE status IN ('sent', 'failed')",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    /// Delete all outbox rows.
    pub fn clear_all(&self) -> Result<u32, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let n = conn
            .execute("DELETE FROM contributions", [])
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    pub fn counts(&self) -> Result<OutboxCounts, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contributions WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let failed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contributions WHERE status = 'failed'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let sent: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contributions WHERE status = 'sent'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(OutboxCounts {
            pending: pending as u32,
            failed: failed as u32,
            sent: sent as u32,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct OutboxCounts {
    pub pending: u32,
    pub failed: u32,
    pub sent: u32,
}
