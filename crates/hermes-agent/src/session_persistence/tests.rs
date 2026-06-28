use super::*;
use hermes_core::Message;
use rusqlite::params;

fn mark_session_old(sp: &SessionPersistence, session_id: &str, days_old: i64) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    let ts = (Utc::now() - ChronoDuration::days(days_old)).to_rfc3339();
    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        params![ts, session_id],
    )
    .expect("update updated_at");
}

fn set_session_lineage(
    sp: &SessionPersistence,
    session_id: &str,
    parent_session_id: Option<&str>,
    end_reason: Option<&str>,
    created_at: &str,
    ended_at: Option<&str>,
) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions
         SET parent_session_id = ?1,
             end_reason = ?2,
             created_at = ?3,
             updated_at = ?3,
             ended_at = ?4
         WHERE id = ?5",
        params![
            parent_session_id,
            end_reason,
            created_at,
            ended_at,
            session_id
        ],
    )
    .expect("update session lineage");
}

fn set_session_model_config(sp: &SessionPersistence, session_id: &str, model_config: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions SET model_config = ?1 WHERE id = ?2",
        params![model_config, session_id],
    )
    .expect("update model config");
}

fn set_session_platform(sp: &SessionPersistence, session_id: &str, platform: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions SET platform = ?1 WHERE id = ?2",
        params![platform, session_id],
    )
    .expect("update platform");
}

fn set_message_created_at(sp: &SessionPersistence, session_id: &str, created_at: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE messages SET created_at = ?1 WHERE session_id = ?2",
        params![created_at, session_id],
    )
    .expect("update message timestamp");
}

fn grow_sessions_wal(sp: &SessionPersistence) -> (rusqlite::Connection, std::path::PathBuf, u64) {
    sp.ensure_db().unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    conn.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS wal_growth (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            payload TEXT NOT NULL
        )",
        [],
    )
    .unwrap();

    let payload = "x".repeat(64 * 1024);
    let tx = conn.unchecked_transaction().unwrap();
    for _ in 0..96 {
        tx.execute(
            "INSERT INTO wal_growth (payload) VALUES (?1)",
            params![&payload],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let wal_path = std::path::PathBuf::from(format!("{}-wal", sp.db_path.display()));
    let wal_len = std::fs::metadata(&wal_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    assert!(
        wal_len > 0,
        "test setup should create a non-empty sessions.db-wal sidecar"
    );
    (conn, wal_path, wal_len)
}

fn wal_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn fts_match_rows(sp: &SessionPersistence, query: &str) -> Vec<(i64, String)> {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    if !SessionPersistence::fts_table_exists(&conn).unwrap() {
        return Vec::new();
    }
    let mut stmt = conn
        .prepare(
            "SELECT rowid, snippet(messages_fts, 0, '[', ']', '...', 8)
             FROM messages_fts
             WHERE messages_fts MATCH ?1
             ORDER BY rowid",
        )
        .unwrap();
    stmt.query_map(params![query], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn persist_searchable_session(sp: &SessionPersistence) {
    let messages = vec![
        Message::user("hello world"),
        Message::assistant("reply about pizza"),
        Message::user("second turn"),
        Message::assistant("more pizza details"),
    ];
    sp.persist_session("schema-repair", &messages, Some("gpt-4o"), None, None, None)
        .unwrap();
    assert_eq!(fts_match_rows(sp, "pizza").len(), 2);
}

fn corrupt_duplicate_fts_schema(sp: &SessionPersistence) {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    conn.execute(
        "INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
         SELECT type, name, tbl_name, rootpage, sql
         FROM sqlite_master
         WHERE name = 'messages_fts'",
        [],
    )
    .unwrap();
    let _ = conn.execute_batch("PRAGMA writable_schema=OFF");
    drop(conn);
}

#[test]
fn malformed_schema_error_classifier_matches_expected_sqlite_messages() {
    assert!(SessionPersistence::is_malformed_db_error_message(
        "malformed database schema (messages_fts) - table messages_fts already exists"
    ));
    assert!(SessionPersistence::is_malformed_db_error_message(
        "database disk image is malformed"
    ));
    assert!(!SessionPersistence::is_malformed_db_error_message(
        "database is locked"
    ));
}

#[test]
fn duplicate_fts_schema_makes_first_statement_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let err = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .unwrap_err();
    assert!(SessionPersistence::is_malformed_sqlite_error(&err));
}

#[test]
fn repair_malformed_schema_preserves_sessions_messages_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    let report = sp.repair_malformed_schema(true);
    assert!(report.repaired, "{report:?}");
    assert_eq!(report.strategy.as_deref(), Some("dedup_schema"));
    assert!(report.backup_path.as_ref().is_some_and(|p| p.exists()));
    assert!(sp.db_health_error().is_none());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 1);
    assert_eq!(message_count, 4);
    drop(conn);
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn ensure_db_auto_heals_duplicate_fts_schema_once() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    sp.ensure_db().unwrap();
    assert!(sp.db_health_error().is_none());
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn drop_fts_repair_rebuilds_search_from_canonical_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    sp.repair_schema_drop_fts_pass().unwrap();
    assert!(sp.db_health_error().is_none());
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn unrepairable_sessions_db_fails_safely_with_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    std::fs::create_dir_all(tmp.path()).unwrap();
    std::fs::write(
        &sp.db_path,
        b"SQLite format 3\0not actually a valid database",
    )
    .unwrap();

    let report = sp.repair_malformed_schema(true);
    assert!(!report.repaired);
    assert!(report.error.as_deref().is_some_and(|e| !e.is_empty()));
    assert!(report.backup_path.as_ref().is_some_and(|p| p.exists()));
}

#[test]
fn test_persist_and_load_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let mut assistant = Message::assistant("Hi there!");
    assistant.reasoning_content = Some("provider scratchpad".to_string());
    let messages = vec![
        Message::system("You are helpful"),
        Message::user("Hello"),
        assistant,
    ];

    sp.persist_session(
        "test-session-1",
        &messages,
        Some("gpt-4o"),
        None,
        Some("Test"),
        Some("cached system blob"),
    )
    .unwrap();

    assert_eq!(
        sp.get_system_prompt("test-session-1").unwrap().as_deref(),
        Some("cached system blob")
    );

    let loaded = sp.load_session("test-session-1").unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].role, MessageRole::System);
    assert_eq!(loaded[1].content.as_deref(), Some("Hello"));
    assert_eq!(loaded[2].content.as_deref(), Some("Hi there!"));
    assert_eq!(
        loaded[2].reasoning_content.as_deref(),
        Some("provider scratchpad")
    );
}

#[test]
fn test_persist_and_load_tool_result_name() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::tool_result_with_name(
        "call_terminal",
        "terminal",
        "stdout",
    )];

    sp.persist_session("tool-name-session", &messages, None, None, None, None)
        .unwrap();

    let loaded = sp.load_session("tool-name-session").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].role, MessageRole::Tool);
    assert_eq!(loaded[0].tool_call_id.as_deref(), Some("call_terminal"));
    assert_eq!(loaded[0].name.as_deref(), Some("terminal"));
    assert_eq!(loaded[0].content.as_deref(), Some("stdout"));
}

#[test]
fn test_migrates_reasoning_content_column_for_legacy_db() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("sessions.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            model TEXT,
            platform TEXT DEFAULT 'cli',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            title TEXT,
            message_count INTEGER DEFAULT 0
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            created_at TEXT NOT NULL
        );",
    )
    .unwrap();
    drop(conn);

    let sp = SessionPersistence::new(tmp.path());
    let mut assistant = Message::assistant("legacy");
    assistant.reasoning_content = Some("legacy-think".to_string());
    sp.persist_session("legacy-migrate", &[assistant], None, None, None, None)
        .unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut sessions_stmt = conn
        .prepare("PRAGMA table_info(sessions)")
        .expect("sessions pragma prepare");
    let session_cols = sessions_stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for column in [
        "system_prompt",
        "parent_session_id",
        "model_config",
        "end_reason",
        "ended_at",
    ] {
        assert!(
            session_cols.iter().any(|name| name == column),
            "missing migrated sessions.{column}"
        );
    }
    let mut stmt = conn
        .prepare("PRAGMA table_info(messages)")
        .expect("pragma prepare");
    let message_cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(message_cols.iter().any(|name| name == "reasoning_content"));
    assert!(message_cols.iter().any(|name| name == "name"));

    let loaded = sp.load_session("legacy-migrate").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].reasoning_content.as_deref(), Some("legacy-think"));
}

#[test]
fn test_save_session_log() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages = vec![Message::user("What is 2+2?"), Message::assistant("4")];

    let path = sp
        .save_session_log("log-test", &messages, Some("gpt-4o"))
        .unwrap();

    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("What is 2+2?"));
    assert!(content.contains("👤 User"));
    assert!(content.contains("🤖 Assistant"));
}

#[test]
fn test_save_trajectory() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages = vec![
        Message::user("Build a website"),
        Message::assistant("Sure, I'll help with that."),
    ];

    let path = sp
        .save_trajectory("traj-test", &messages, "Build a website", true)
        .unwrap();

    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["completed"], true);
    assert_eq!(parsed["user_query"], "Build a website");
}

#[test]
fn test_load_nonexistent_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.ensure_db().unwrap();

    let loaded = sp.load_session("nonexistent").unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_persist_replaces_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages1 = vec![Message::user("First")];
    sp.persist_session("replace-test", &messages1, None, None, None, None)
        .unwrap();

    let messages2 = vec![
        Message::user("First"),
        Message::assistant("Response"),
        Message::user("Second"),
    ];
    sp.persist_session("replace-test", &messages2, None, None, None, None)
        .unwrap();

    let loaded = sp.load_session("replace-test").unwrap();
    assert_eq!(loaded.len(), 3);
}

#[test]
fn test_persist_session_updates_existing_model() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("hello")];

    sp.persist_session(
        "model-update",
        &messages,
        Some("openai:gpt-4o"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "model-update",
        &messages,
        Some("nous:hermes-4"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let model: String = conn
        .query_row(
            "SELECT model FROM sessions WHERE id='model-update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "nous:hermes-4");
}

#[test]
fn resolve_resume_session_id_follows_compression_tip_with_nonempty_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("pre-compression turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "cont",
        &[Message::assistant("post-compression reply")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + ChronoDuration::seconds(10)).to_rfc3339();
    let cont_created = (base + ChronoDuration::seconds(20)).to_rfc3339();
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "cont", Some("root"), None, &cont_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "cont");
}

#[test]
fn resolve_resume_session_id_ignores_non_compression_branch_child() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("parent turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "branch",
        &[Message::assistant("delegated work")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let branch_created = (base + ChronoDuration::seconds(20)).to_rfc3339();
    set_session_lineage(&sp, "root", None, None, &root_created, None);
    set_session_lineage(&sp, "branch", Some("root"), None, &branch_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "root");
}

#[test]
fn resolve_resume_session_id_follows_child_created_before_parent_end() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("parent turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "early",
        &[Message::assistant("older branch")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + ChronoDuration::seconds(30)).to_rfc3339();
    let early_created = (base + ChronoDuration::seconds(10)).to_rfc3339();
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "early", Some("root"), None, &early_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "early");
}

#[test]
fn resolve_resume_session_id_prefers_live_race_continuation_over_closed_stale_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for (id, content) in [
        ("root", "parent turn"),
        ("real", "real continuation"),
        ("stale", "stale websocket sibling"),
    ] {
        sp.persist_session(
            id,
            &[Message::assistant(content)],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let real_created = (base + ChronoDuration::seconds(10)).to_rfc3339();
    let root_ended = (base + ChronoDuration::seconds(30)).to_rfc3339();
    let stale_created = (base + ChronoDuration::seconds(40)).to_rfc3339();
    let stale_ended = (base + ChronoDuration::seconds(50)).to_rfc3339();
    let real_message_at = (base + ChronoDuration::seconds(60)).to_rfc3339();
    let stale_message_at = (base + ChronoDuration::seconds(70)).to_rfc3339();
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "real", Some("root"), None, &real_created, None);
    set_session_lineage(
        &sp,
        "stale",
        Some("root"),
        None,
        &stale_created,
        Some(&stale_ended),
    );
    set_message_created_at(&sp, "real", &real_message_at);
    set_message_created_at(&sp, "stale", &stale_message_at);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "real");
}

#[test]
fn resolve_resume_session_id_excludes_branch_delegate_and_tool_children() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for id in ["root", "branch", "delegate", "tool", "cont"] {
        sp.persist_session(
            id,
            &[Message::assistant(format!("{id} message"))],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + ChronoDuration::seconds(10)).to_rfc3339();
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    for (offset, id) in [(20, "cont"), (30, "branch"), (40, "delegate"), (50, "tool")] {
        let created = (base + ChronoDuration::seconds(offset)).to_rfc3339();
        set_session_lineage(&sp, id, Some("root"), None, &created, None);
        let msg_created = (base + ChronoDuration::seconds(offset + 100)).to_rfc3339();
        set_message_created_at(&sp, id, &msg_created);
    }
    set_session_model_config(&sp, "branch", r#"{"_branched_from":"root"}"#);
    set_session_model_config(&sp, "delegate", r#"{"_delegate_from":"root"}"#);
    set_session_platform(&sp, "tool", "tool");

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "cont");
}

#[test]
fn resolve_resume_session_id_walks_compression_chain_to_latest_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for id in ["root", "mid", "tip"] {
        sp.persist_session(
            id,
            &[Message::assistant(format!("{id} message"))],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Utc::now() - ChronoDuration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + ChronoDuration::seconds(10)).to_rfc3339();
    let mid_created = (base + ChronoDuration::seconds(20)).to_rfc3339();
    let mid_ended = (base + ChronoDuration::seconds(30)).to_rfc3339();
    let tip_created = (base + ChronoDuration::seconds(40)).to_rfc3339();
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(
        &sp,
        "mid",
        Some("root"),
        Some("compression"),
        &mid_created,
        Some(&mid_ended),
    );
    set_session_lineage(&sp, "tip", Some("mid"), None, &tip_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "tip");
}

#[test]
fn delete_session_if_empty_deletes_empty_untitled_row_and_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session("empty", &[], Some("gpt-4o"), Some("cli"), None, None)
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    std::fs::write(
        tmp.path().join("sessions").join("empty.json"),
        r#"{"session_info":{"session_id":"empty"},"messages":[]}"#,
    )
    .unwrap();

    assert!(sp.delete_session_if_empty("empty").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id='empty'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert!(!tmp.path().join("sessions").join("empty.json").exists());
}

#[test]
fn delete_session_if_empty_preserves_sessions_with_messages_title_or_children() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "with-message",
        &[Message::user("hello")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session("titled", &[], None, Some("cli"), Some("Plans"), None)
        .unwrap();
    sp.persist_session("parent", &[], None, Some("cli"), None, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sessions (id, model, platform, created_at, updated_at, parent_session_id)
         VALUES ('child', 'gpt-4o', 'cli', ?1, ?1, 'parent')",
        rusqlite::params![now],
    )
    .unwrap();
    drop(conn);

    assert!(!sp.delete_session_if_empty("with-message").unwrap());
    assert!(!sp.delete_session_if_empty("titled").unwrap());
    assert!(!sp.delete_session_if_empty("parent").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    for session_id in ["with-message", "titled", "parent", "child"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{session_id} should remain");
    }
}

#[test]
fn delete_session_if_empty_keeps_nonempty_snapshot_file() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session("empty-db", &[], Some("gpt-4o"), Some("cli"), None, None)
        .unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let snapshot_path = sessions_dir.join("empty-db.json");
    std::fs::write(
        &snapshot_path,
        r#"{"session_info":{"session_id":"empty-db"},"messages":[{"role":"User","content":"keep"}]}"#,
    )
    .unwrap();

    assert!(sp.delete_session_if_empty("empty-db").unwrap());
    assert!(snapshot_path.exists());
}

#[test]
fn test_update_session_model_only_updates_existing_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("hello")];

    assert!(!sp
        .update_session_model("missing-session", "openai:gpt-4o")
        .unwrap());

    sp.persist_session(
        "existing-session",
        &messages,
        Some("openai:gpt-4o"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    assert!(sp
        .update_session_model("existing-session", "anthropic:claude-sonnet")
        .unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let model: String = conn
        .query_row(
            "SELECT model FROM sessions WHERE id='existing-session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "anthropic:claude-sonnet");
}

#[test]
fn test_rewind_soft_deletes_n_user_turns_and_loads_only_active_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![
        Message::system("sys"),
        Message::user("question 1"),
        Message::assistant("answer 1"),
        Message::user("question 2"),
        Message::assistant("answer 2"),
        Message::user("question 3"),
        Message::assistant("answer 3"),
    ];
    sp.persist_session("rewind-n", &messages, None, None, None, None)
        .unwrap();

    let recent = sp.list_recent_user_messages("rewind-n", 2).unwrap();
    assert_eq!(
        recent
            .iter()
            .filter_map(|row| row.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["question 3", "question 2"]
    );

    let outcome = sp.rewind_active_user_turns("rewind-n", 2).unwrap().unwrap();
    assert_eq!(outcome.target_content.as_deref(), Some("question 2"));
    assert_eq!(outcome.inactive_count, 4);
    assert_eq!(outcome.active_message_count, 3);
    assert_eq!(outcome.rewind_count, 1);

    let loaded = sp.load_session("rewind-n").unwrap();
    assert_eq!(
        loaded
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["sys", "question 1", "answer 1"]
    );

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let inactive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='rewind-n' AND active=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inactive, 4);
    let rewind_count: i64 = conn
        .query_row(
            "SELECT rewind_count FROM sessions WHERE id='rewind-n'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rewind_count, 1);

    assert_eq!(
        sp.restore_rewound_since("rewind-n", outcome.target_message_id)
            .unwrap(),
        4
    );
    assert_eq!(sp.load_session("rewind-n").unwrap().len(), messages.len());
}

#[test]
fn test_persist_session_preserves_inactive_rewind_audit_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![
        Message::user("keep"),
        Message::assistant("kept"),
        Message::user("rewind me"),
        Message::assistant("rewound"),
    ];
    sp.persist_session("audit-preserve", &messages, None, None, None, None)
        .unwrap();
    sp.rewind_active_user_turns("audit-preserve", 1)
        .unwrap()
        .unwrap();

    sp.persist_session(
        "audit-preserve",
        &[Message::user("keep"), Message::assistant("replacement")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let inactive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='audit-preserve' AND active=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inactive, 2);
    assert_eq!(sp.load_session("audit-preserve").unwrap().len(), 2);
}

#[test]
fn test_replacing_messages_clears_old_fts_rows_when_available() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    sp.persist_session(
        "fts-replace",
        &[Message::user("needlebefore")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "fts-replace",
        &[Message::user("needleafter")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    if SessionPersistence::fts_table_exists(&conn).unwrap() {
        let old_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needlebefore'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needleafter'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_hits, 0);
        assert_eq!(new_hits, 1);
    }
}

#[test]
fn test_delete_fts_rows_ignores_missing_fts_table() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            created_at TEXT NOT NULL
        );",
    )
    .unwrap();

    SessionPersistence::delete_fts_rows_for_session(&conn, "no-fts").unwrap();
}

#[test]
fn test_fts_unavailable_error_classifier_matches_missing_modules() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let err = conn
        .execute_batch("CREATE VIRTUAL TABLE broken_fts USING definitely_missing_module(content);")
        .expect_err("missing virtual table module should fail");
    assert!(SessionPersistence::is_fts5_unavailable_error(&err));
}

#[test]
fn test_optimize_fts_returns_existing_index_count() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-count",
        &[Message::user("hello optimized world")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());
    assert_eq!(sp.optimize_fts().unwrap(), expected);
}

#[test]
fn test_optimize_fts_preserves_search_rows_and_snippets() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = (0..50)
        .map(|idx| Message::user(format!("needle alpha bravo charlie message {idx}")))
        .collect::<Vec<_>>();
    sp.persist_session("opt-search", &messages, None, None, None, None)
        .unwrap();

    let before = fts_match_rows(&sp, "needle");
    if before.is_empty() {
        assert_eq!(sp.optimize_fts().unwrap(), 0);
        return;
    }

    assert_eq!(sp.optimize_fts().unwrap(), 1);
    let after = fts_match_rows(&sp, "needle");
    assert_eq!(after, before);
    assert!(after
        .iter()
        .all(|(_, snippet)| snippet.contains("[needle]")));
}

#[test]
fn test_optimize_fts_skips_missing_optional_trigram_table() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-missing-trigram",
        &[Message::user("trigram optional")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    assert!(!SessionPersistence::named_fts_table_exists(&conn, "messages_fts_trigram").unwrap());
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());
    assert_eq!(sp.optimize_fts().unwrap(), expected);
}

#[test]
fn test_optimize_fts_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-idempotent",
        &[Message::user("repeatable optimize marker")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());

    assert_eq!(sp.optimize_fts().unwrap(), expected);
    assert_eq!(sp.optimize_fts().unwrap(), expected);
    assert_eq!(sp.load_session("opt-idempotent").unwrap().len(), 1);
    if expected > 0 {
        assert_eq!(fts_match_rows(&sp, "repeatable").len(), 1);
    }
}

#[test]
fn test_vacuum_runs_after_fts_optimize_without_changing_search() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "vacuum-opt",
        &[Message::user("vacuum needle still searchable")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let before = fts_match_rows(&sp, "needle");

    sp.vacuum().unwrap();

    assert_eq!(fts_match_rows(&sp, "needle"), before);
}

#[test]
fn test_state_meta_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    assert_eq!(sp.get_meta("nope").unwrap(), None);
    sp.set_meta("k1", "v1").unwrap();
    assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v1"));
    sp.set_meta("k1", "v2").unwrap();
    assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v2"));
}

#[test]
fn test_truncate_wal_checkpoint_shrinks_sessions_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let (_conn, wal_path, before) = grow_sessions_wal(&sp);

    sp.truncate_wal_checkpoint().unwrap();

    assert_eq!(
        wal_len(&wal_path),
        0,
        "TRUNCATE checkpoint should shrink WAL from {before} bytes"
    );
}

#[test]
fn test_auto_maintenance_truncates_wal_after_meta_write() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let (_conn, wal_path, before) = grow_sessions_wal(&sp);

    let result = sp.maybe_auto_prune_and_vacuum(90, 0, false);

    assert!(!result.skipped);
    assert_eq!(result.pruned, 0);
    assert!(result.error.is_none());
    assert_eq!(
        wal_len(&wal_path),
        0,
        "auto-maintenance should truncate WAL from {before} bytes"
    );
}

#[test]
fn test_auto_maintenance_prunes_and_vacuums() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let old_messages = vec![Message::user("old"), Message::assistant("done")];
    let new_messages = vec![Message::user("fresh"), Message::assistant("ok")];

    sp.persist_session("old-1", &old_messages, None, None, None, None)
        .unwrap();
    sp.persist_session("old-2", &old_messages, None, None, None, None)
        .unwrap();
    sp.persist_session("fresh-1", &new_messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    mark_session_old(&sp, "old-2", 100);

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 2);
    assert!(result.vacuumed);
    assert!(result.error.is_none());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn test_auto_maintenance_respects_interval_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("old"), Message::assistant("done")];

    sp.persist_session("old-1", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    let first = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(!first.skipped);
    assert_eq!(first.pruned, 1);

    sp.persist_session("old-2", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-2", 100);
    let second = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(second.skipped);
    assert_eq!(second.pruned, 0);

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let still_there: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='old-2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(still_there, 1);
}

#[test]
fn test_auto_maintenance_no_prunable_rows_skips_vacuum() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("fresh"), Message::assistant("ok")];
    sp.persist_session("fresh-1", &messages, None, None, None, None)
        .unwrap();

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 0);
    assert!(!result.vacuumed);
    assert!(sp.get_meta("last_auto_prune").unwrap().is_some());
}

#[test]
fn test_auto_maintenance_corrupt_marker_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("old"), Message::assistant("done")];
    sp.persist_session("old-1", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    sp.set_meta("last_auto_prune", "not-a-timestamp").unwrap();

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 1);
}
