//! Job persistence layer for the cron scheduler.
//!
//! Provides a trait for persisting cron jobs and a file-based implementation
//! that stores jobs as JSON files in `~/.hermes/cron/`.

use std::path::PathBuf;
use std::{collections::HashSet, ffi::OsStr};

use async_trait::async_trait;
use tokio::fs;

use crate::job::CronJob;

// ---------------------------------------------------------------------------
// JobPersistence trait
// ---------------------------------------------------------------------------

/// Trait for persisting cron job state.
#[async_trait]
pub trait JobPersistence: Send + Sync {
    /// Save all jobs (bulk replace).
    async fn save_jobs(&self, jobs: &[CronJob]) -> Result<(), CronPersistenceError>;

    /// Load all persisted jobs.
    async fn load_jobs(&self) -> Result<Vec<CronJob>, CronPersistenceError>;

    /// Save or update a single job.
    async fn save_job(&self, job: &CronJob) -> Result<(), CronPersistenceError>;

    /// Delete a single job by ID.
    async fn delete_job(&self, id: &str) -> Result<(), CronPersistenceError>;
}

// ---------------------------------------------------------------------------
// CronPersistenceError
// ---------------------------------------------------------------------------

/// Errors that can occur during persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum CronPersistenceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Corrupted persistence data: {0}")]
    Corrupted(String),

    #[error("SQLite error: {0}")]
    Sqlite(String),
}

// ---------------------------------------------------------------------------
// FileJobPersistence
// ---------------------------------------------------------------------------

/// File-based job persistence. Stores each job as a JSON file in `data_dir`.
///
/// Directory structure:
/// ```text
/// data_dir/
///   <job-id>.json
///   <job-id>.json
/// ```
#[derive(Debug, Clone)]
pub struct FileJobPersistence {
    data_dir: PathBuf,
}

impl FileJobPersistence {
    /// Create a new file persistence instance using the default data directory.
    ///
    /// The default location is determined by the `directories` crate:
    /// - macOS: `~/Library/Application Support/hermes/cron/`
    /// - Linux: `~/.local/share/hermes/cron/`
    /// - Windows: `C:\Users\{User}\AppData\Roaming\hermes\cron\`
    pub fn new() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }

    /// Create a file persistence instance with a custom data directory.
    pub fn with_dir(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Return the data directory path.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Ensure the data directory exists.
    async fn ensure_dir(&self) -> Result<(), std::io::Error> {
        fs::create_dir_all(&self.data_dir).await
    }

    /// Return the file path for a given job ID.
    fn job_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.json", id))
    }

    /// Return a temporary file path for atomic writes.
    fn temp_job_path(&self, id: &str) -> PathBuf {
        self.data_dir
            .join(format!(".{}.{}.tmp", id, uuid::Uuid::new_v4()))
    }

    /// Atomically write a job file by writing to a temporary file and renaming.
    async fn atomic_write_job(&self, id: &str, contents: &str) -> Result<(), std::io::Error> {
        let tmp = self.temp_job_path(id);
        let dst = self.job_path(id);
        fs::write(&tmp, contents).await?;
        fs::rename(&tmp, &dst).await?;
        Ok(())
    }
}

impl Default for FileJobPersistence {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JobPersistence for FileJobPersistence {
    async fn save_jobs(&self, jobs: &[CronJob]) -> Result<(), CronPersistenceError> {
        self.ensure_dir().await?;

        // Bulk save is treated as a replace operation:
        // 1) atomically write each desired job
        // 2) remove stale JSON files no longer present in `jobs`
        let mut keep_ids = HashSet::new();
        for job in jobs {
            let contents = serde_json::to_string_pretty(job)?;
            self.atomic_write_job(&job.id, &contents).await?;
            keep_ids.insert(job.id.clone());
        }

        let mut entries = fs::read_dir(&self.data_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !keep_ids.contains(stem) {
                fs::remove_file(path).await?;
            }
        }
        Ok(())
    }

    async fn load_jobs(&self) -> Result<Vec<CronJob>, CronPersistenceError> {
        self.ensure_dir().await?;
        let mut jobs = Vec::new();

        let mut entries = fs::read_dir(&self.data_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                let contents = fs::read_to_string(&path).await?;
                let job = serde_json::from_str::<CronJob>(&contents).map_err(|e| {
                    CronPersistenceError::Corrupted(format!(
                        "failed to parse job file {}: {}",
                        path.display(),
                        e
                    ))
                })?;
                jobs.push(job);
            }
        }

        Ok(jobs)
    }

    async fn save_job(&self, job: &CronJob) -> Result<(), CronPersistenceError> {
        self.ensure_dir().await?;
        let contents = serde_json::to_string_pretty(job)?;
        self.atomic_write_job(&job.id, &contents).await?;
        Ok(())
    }

    async fn delete_job(&self, id: &str) -> Result<(), CronPersistenceError> {
        let path = self.job_path(id);
        if fs::try_exists(&path).await.unwrap_or(false) {
            fs::remove_file(path).await?;
        }
        Ok(())
    }
}

/// Return the user's data directory for Hermes.
///
/// On macOS: ~/Library/Application Support/hermes
/// On Linux: ~/.local/share/hermes
/// On Windows: C:\Users\{User}\AppData\Roaming\hermes
fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "hermes", "hermes")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cron")
}

// ---------------------------------------------------------------------------
// SqliteJobPersistence
// ---------------------------------------------------------------------------

/// SQLite-based job persistence. Stores all jobs in a single `cron_jobs` table.
///
/// This is more robust than file-based persistence for concurrent access
/// and provides atomic operations.
#[derive(Debug)]
pub struct SqliteJobPersistence {
    db_path: PathBuf,
}

impl SqliteJobPersistence {
    /// Create a new SQLite persistence instance using the default data directory.
    pub fn new() -> Self {
        let dir = default_data_dir();
        Self {
            db_path: dir.join("cron_jobs.db"),
        }
    }

    /// Create a SQLite persistence instance with a custom database path.
    pub fn with_path(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    /// Open a connection and ensure the schema exists.
    fn open_connection(&self) -> Result<rusqlite::Connection, CronPersistenceError> {
        // Ensure parent directory exists
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CronPersistenceError::Io(e))?;
        }

        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cron_jobs (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL
            );",
        )
        .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        Ok(conn)
    }
}

impl Default for SqliteJobPersistence {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JobPersistence for SqliteJobPersistence {
    async fn save_jobs(&self, jobs: &[CronJob]) -> Result<(), CronPersistenceError> {
        let conn = self.open_connection()?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        // Clear existing and re-insert all
        tx.execute("DELETE FROM cron_jobs", [])
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        for job in jobs {
            let data = serde_json::to_string(job)?;
            tx.execute(
                "INSERT INTO cron_jobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![job.id, data],
            )
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        Ok(())
    }

    async fn load_jobs(&self) -> Result<Vec<CronJob>, CronPersistenceError> {
        let conn = self.open_connection()?;

        let mut stmt = conn
            .prepare("SELECT data FROM cron_jobs")
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;
        let mut jobs = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?
        {
            let data: String = row
                .get(0)
                .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;
            let job = serde_json::from_str::<CronJob>(&data).map_err(|e| {
                CronPersistenceError::Corrupted(format!("failed to parse sqlite cron job row: {e}"))
            })?;
            jobs.push(job);
        }

        Ok(jobs)
    }

    async fn save_job(&self, job: &CronJob) -> Result<(), CronPersistenceError> {
        let conn = self.open_connection()?;
        let data = serde_json::to_string(job)?;

        conn.execute(
            "INSERT OR REPLACE INTO cron_jobs (id, data) VALUES (?1, ?2)",
            rusqlite::params![job.id, data],
        )
        .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        Ok(())
    }

    async fn delete_job(&self, id: &str) -> Result<(), CronPersistenceError> {
        let conn = self.open_connection()?;

        conn.execute("DELETE FROM cron_jobs WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| CronPersistenceError::Sqlite(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_save_and_load_job() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());

        let job = crate::job::CronJob::new("0 9 * * *", "Test job");
        persistence.save_job(&job).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job.id);
        assert_eq!(loaded[0].prompt, "Test job");
    }

    #[tokio::test]
    async fn test_delete_job() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());

        let job = crate::job::CronJob::new("0 9 * * *", "Test job");
        persistence.save_job(&job).await.unwrap();
        persistence.delete_job(&job.id).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_save_jobs_bulk() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());

        let job1 = crate::job::CronJob::new("0 9 * * *", "Job 1");
        let job2 = crate::job::CronJob::new("0 10 * * *", "Job 2");
        persistence
            .save_jobs(&[job1.clone(), job2.clone()])
            .await
            .unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_save_jobs_bulk_removes_stale_entries() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());

        let job1 = crate::job::CronJob::new("0 9 * * *", "Job 1");
        let job2 = crate::job::CronJob::new("0 10 * * *", "Job 2");
        persistence
            .save_jobs(&[job1.clone(), job2.clone()])
            .await
            .unwrap();

        // Bulk replace with only one job should remove stale json files.
        persistence.save_jobs(&[job1.clone()]).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job1.id);
    }

    #[tokio::test]
    async fn test_save_job_atomic_write_no_temp_files_left() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());
        let job = crate::job::CronJob::new("0 9 * * *", "Atomic write");
        persistence.save_job(&job).await.unwrap();

        let mut entries = fs::read_dir(dir.path()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(!name.ends_with(".tmp"), "unexpected temp file: {name}");
        }
    }

    #[tokio::test]
    async fn test_file_load_jobs_fails_on_corrupt_json() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());

        let corrupt_path = dir.path().join("corrupt.json");
        fs::write(&corrupt_path, "{not-valid-json").await.unwrap();

        let err = persistence
            .load_jobs()
            .await
            .expect_err("must fail on corruption");
        assert!(
            matches!(err, CronPersistenceError::Corrupted(ref msg) if msg.contains("corrupt.json")),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let dir = tempdir().unwrap();
        let persistence = FileJobPersistence::with_dir(dir.path().to_path_buf());
        // Deleting a nonexistent job should not error
        persistence.delete_job("nonexistent-id").await.unwrap();
    }

    // --- SQLite persistence tests ---

    #[tokio::test]
    async fn test_sqlite_save_and_load_job() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);

        let job = crate::job::CronJob::new("0 9 * * *", "SQLite test job");
        persistence.save_job(&job).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, job.id);
        assert_eq!(loaded[0].prompt, "SQLite test job");
    }

    #[tokio::test]
    async fn test_sqlite_delete_job() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);

        let job = crate::job::CronJob::new("0 9 * * *", "Delete me");
        persistence.save_job(&job).await.unwrap();
        persistence.delete_job(&job.id).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_sqlite_save_jobs_bulk() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);

        let job1 = crate::job::CronJob::new("0 9 * * *", "Job 1");
        let job2 = crate::job::CronJob::new("0 10 * * *", "Job 2");
        persistence.save_jobs(&[job1, job2]).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_sqlite_upsert() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);

        let mut job = crate::job::CronJob::new("0 9 * * *", "Original");
        persistence.save_job(&job).await.unwrap();

        job.prompt = "Updated".to_string();
        persistence.save_job(&job).await.unwrap();

        let loaded = persistence.load_jobs().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].prompt, "Updated");
    }

    #[tokio::test]
    async fn test_sqlite_load_jobs_fails_on_corrupt_row() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);

        // Initialize schema.
        let _ = persistence.load_jobs().await.unwrap();

        let conn = persistence.open_connection().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO cron_jobs (id, data) VALUES (?1, ?2)",
            rusqlite::params!["corrupt", "{not-valid-json"],
        )
        .unwrap();

        let err = persistence
            .load_jobs()
            .await
            .expect_err("must fail on corruption");
        assert!(
            matches!(err, CronPersistenceError::Corrupted(ref msg) if msg.contains("sqlite cron job row")),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_sqlite_delete_nonexistent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_cron.db");
        let persistence = SqliteJobPersistence::with_path(db_path);
        // Should not error
        persistence.delete_job("nonexistent-id").await.unwrap();
    }
}
