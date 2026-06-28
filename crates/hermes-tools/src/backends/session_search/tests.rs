#[cfg(test)]
mod tests {
    use super::SqliteSessionSearchBackend;
    use crate::tools::session_search::SessionSearchBackend;
    use rusqlite::Connection;
    use serde_json::Value;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn backend_with_db() -> (TempDir, SqliteSessionSearchBackend) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("sessions.db");
        let backend =
            SqliteSessionSearchBackend::new(db_path.to_str().expect("utf8 path")).expect("backend");
        (tmp, backend)
    }

    fn db_conn(tmp: &TempDir) -> Connection {
        Connection::open(tmp.path().join("sessions.db")).expect("open db")
    }

    fn no_fts_backend_with_seeded_db() -> SqliteSessionSearchBackend {
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                parent_session_id TEXT,
                model_config TEXT,
                end_reason TEXT,
                ended_at TEXT,
                rewind_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                created_at TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX idx_messages_session ON messages(session_id);",
        )
        .expect("schema");
        insert_session(
            &conn,
            "nofts-session",
            None,
            None,
            None,
            None,
            "2026-01-01T00:00:00Z",
        );
        insert_message(
            &conn,
            "nofts-session",
            "user",
            "uniquephrase only exists in plain messages",
        );
        SqliteSessionSearchBackend {
            conn: Mutex::new(conn),
            fts_enabled: false,
        }
    }

    fn insert_session(
        conn: &Connection,
        id: &str,
        parent: Option<&str>,
        model_config: Option<&str>,
        end_reason: Option<&str>,
        ended_at: Option<&str>,
        created_at: &str,
    ) {
        conn.execute(
            "INSERT INTO sessions (
                id, model, platform, created_at, updated_at, title, message_count,
                parent_session_id, model_config, end_reason, ended_at
             )
             VALUES (?1, 'test/model', 'cli', ?6, ?6, ?1, 0, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, parent, model_config, end_reason, ended_at, created_at],
        )
        .expect("insert session");
    }

    fn insert_message(conn: &Connection, session_id: &str, role: &str, content: &str) {
        insert_message_at(conn, session_id, role, content, "2026-01-01T00:00:30Z");
    }

    fn insert_message_at(
        conn: &Connection,
        session_id: &str,
        role: &str,
        content: &str,
        timestamp: &str,
    ) {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role, content, timestamp],
        )
        .expect("insert message");
        conn.execute(
            "UPDATE sessions SET message_count = (
                SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1
             ) WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .expect("update message count");
    }

    fn update_session_model_config(conn: &Connection, session_id: &str, model_config: &str) {
        conn.execute(
            "UPDATE sessions SET model_config = ?1 WHERE id = ?2",
            rusqlite::params![model_config, session_id],
        )
        .expect("update model_config");
    }

    fn update_session_platform(conn: &Connection, session_id: &str, platform: &str) {
        conn.execute(
            "UPDATE sessions SET platform = ?1 WHERE id = ?2",
            rusqlite::params![platform, session_id],
        )
        .expect("update platform");
    }

    fn insert_inactive_message(conn: &Connection, session_id: &str, role: &str, content: &str) {
        insert_message(conn, session_id, role, content);
        conn.execute(
            "UPDATE messages SET active = 0
             WHERE id = (SELECT MAX(id) FROM messages WHERE session_id = ?1)",
            rusqlite::params![session_id],
        )
        .expect("mark inactive");
        conn.execute(
            "UPDATE sessions SET message_count = (
                SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1
             ) WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .expect("update active count");
    }

    fn result_ids(output: &str) -> Vec<String> {
        let parsed: Value = serde_json::from_str(output).expect("json output");
        parsed["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|item| {
                item.get("session_id")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .collect()
    }

    fn parsed(output: &str) -> Value {
        serde_json::from_str(output).expect("json output")
    }

    #[test]
    fn format_timestamp_handles_none() {
        assert_eq!(
            SqliteSessionSearchBackend::format_timestamp(None),
            "unknown"
        );
    }

    #[test]
    fn format_timestamp_handles_unix_number() {
        let out = SqliteSessionSearchBackend::format_timestamp(Some("1700000000"));
        assert!(out.contains("2023") || out.contains("2024"));
        assert!(out.contains(" at "));
    }

    #[test]
    fn format_timestamp_handles_rfc3339() {
        let out = SqliteSessionSearchBackend::format_timestamp(Some("2026-01-02T03:04:05Z"));
        assert!(out.contains("2026"));
        assert!(out.contains(" at "));
    }

    #[tokio::test]
    async fn recent_mode_keeps_marked_branch_visible_after_parent_reended() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("tui_shutdown"),
            Some("2026-01-01T00:00:20Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "branch",
            Some("parent"),
            Some(r#"{"_branched_from":"parent"}"#),
            None,
            None,
            "2026-01-01T00:00:10Z",
        );
        insert_message(&conn, "branch", "user", "branch work must stay visible");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(ids.contains(&"branch".to_string()), "{output}");
    }

    #[tokio::test]
    async fn recent_mode_keeps_legacy_branch_visible() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("branched"),
            Some("2026-01-01T00:00:05Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "legacy-branch",
            Some("parent"),
            None,
            None,
            None,
            "2026-01-01T00:00:06Z",
        );
        insert_message(&conn, "legacy-branch", "user", "legacy branch work");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(ids.contains(&"legacy-branch".to_string()), "{output}");
    }

    #[tokio::test]
    async fn recent_mode_hides_unmarked_child_sessions() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("completed"),
            Some("2026-01-01T00:00:05Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "subagent-child",
            Some("parent"),
            None,
            None,
            None,
            "2026-01-01T00:00:01Z",
        );
        insert_message(&conn, "subagent-child", "assistant", "internal child work");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(!ids.contains(&"subagent-child".to_string()), "{output}");
    }

    #[tokio::test]
    async fn search_keeps_marked_branch_result_on_branch_session() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("tui_shutdown"),
            Some("2026-01-01T00:00:20Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_message(&conn, "parent", "user", "parent only message");
        insert_session(
            &conn,
            "branch",
            Some("parent"),
            Some(r#"{"_branched_from":"parent"}"#),
            None,
            None,
            "2026-01-01T00:00:10Z",
        );
        insert_message(&conn, "branch", "user", "unique branch phrase");

        let output = backend
            .search(Some("unique"), None, 5, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids.first().map(String::as_str), Some("branch"), "{output}");
    }

    #[tokio::test]
    async fn search_matches_session_id_before_content_hits() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260603_090200_exact",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(
            &conn,
            "20260603_090200_exact",
            "user",
            "ordinary message without the numeric query",
        );
        insert_session(
            &conn,
            "content-session",
            None,
            None,
            None,
            None,
            "2026-06-03T09:03:00Z",
        );
        insert_message(
            &conn,
            "content-session",
            "assistant",
            "20260603 appears only in this transcript",
        );

        let output = backend
            .search(Some("20260603"), None, 2, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(
            ids,
            vec![
                "20260603_090200_exact".to_string(),
                "content-session".to_string()
            ],
            "{output}"
        );
    }

    #[tokio::test]
    async fn search_sanitizes_colon_queries_for_fts5() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "colon-search-session",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(
            &conn,
            "colon-search-session",
            "user",
            "workspace:hermes contains a literal colon token",
        );

        let output = backend
            .search(Some("workspace:hermes"), None, 5, None)
            .await
            .expect("colon query should not be parsed as an FTS5 column filter");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["colon-search-session".to_string()], "{output}");
    }

    #[tokio::test]
    async fn no_fts_backend_still_supports_session_id_lookup() {
        let backend = no_fts_backend_with_seeded_db();

        let output = backend
            .search(Some("nofts-session"), None, 5, None)
            .await
            .expect("search");
        let value = parsed(&output);

        assert_eq!(result_ids(&output), vec!["nofts-session".to_string()]);
        assert_eq!(value["search_degraded"], "fts5_unavailable");
    }

    #[tokio::test]
    async fn no_fts_backend_degrades_content_search_to_empty_success() {
        let backend = no_fts_backend_with_seeded_db();

        let output = backend
            .search(Some("uniquephrase"), None, 5, None)
            .await
            .expect("search");
        let value = parsed(&output);

        assert_eq!(value["success"], true);
        assert_eq!(value["count"], 0);
        assert_eq!(value["search_degraded"], "fts5_unavailable");
    }

    #[tokio::test]
    async fn search_ignores_inactive_rewound_messages() {
        let (_tmp, backend) = backend_with_db();
        {
            let conn = backend.conn.lock().unwrap();
            insert_session(
                &conn,
                "rewound-search",
                None,
                None,
                None,
                None,
                "2026-01-01T00:00:00Z",
            );
            insert_message(&conn, "rewound-search", "user", "visible active phrase");
            insert_inactive_message(&conn, "rewound-search", "user", "hiddenrewoundphrase");
        }

        let output = backend
            .search(Some("hiddenrewoundphrase"), None, 5, None)
            .await
            .expect("search");
        let value = parsed(&output);

        assert_eq!(value["success"], true);
        assert_eq!(value["count"], 0);
    }

    #[tokio::test]
    async fn recent_preview_uses_latest_active_message() {
        let (_tmp, backend) = backend_with_db();
        {
            let conn = backend.conn.lock().unwrap();
            insert_session(
                &conn,
                "rewound-recent",
                None,
                None,
                None,
                None,
                "2026-01-01T00:00:00Z",
            );
            insert_message(&conn, "rewound-recent", "user", "visible preview");
            insert_inactive_message(&conn, "rewound-recent", "user", "hidden preview");
        }

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let value = parsed(&output);
        let preview = value["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["session_id"] == "rewound-recent")
            .and_then(|row| row["preview"].as_str())
            .unwrap();

        assert_eq!(preview, "visible preview");
    }

    #[tokio::test]
    async fn search_dedupes_session_id_and_content_hits_by_logical_session() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260603_090200_exact",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(
            &conn,
            "20260603_090200_exact",
            "user",
            "20260603 also appears in the content hit",
        );

        let output = backend
            .search(Some("20260603"), None, 5, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["20260603_090200_exact".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_resolves_compression_root_to_tip() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260602_235959_root99",
            None,
            None,
            Some("compression"),
            Some("2026-06-03T00:00:05Z"),
            "2026-06-02T23:59:59Z",
        );
        insert_message(&conn, "20260602_235959_root99", "user", "root segment");
        insert_session(
            &conn,
            "20260603_010000_tip01",
            Some("20260602_235959_root99"),
            None,
            None,
            None,
            "2026-06-03T00:00:06Z",
        );
        insert_message(&conn, "20260603_010000_tip01", "user", "continued segment");

        let output = backend
            .search(Some("root99"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["20260603_010000_tip01".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_resolves_compression_tip_created_before_parent_end() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "race-root",
            None,
            None,
            Some("compression"),
            Some("2026-06-03T00:00:30Z"),
            "2026-06-03T00:00:00Z",
        );
        insert_message(&conn, "race-root", "user", "root segment");
        insert_session(
            &conn,
            "race-cont",
            Some("race-root"),
            None,
            None,
            None,
            "2026-06-03T00:00:10Z",
        );
        insert_message_at(
            &conn,
            "race-cont",
            "user",
            "continuation persisted before parent ended_at",
            "2026-06-03T00:00:40Z",
        );

        let output = backend
            .search(Some("race-root"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["race-cont".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_prefers_live_race_continuation_over_closed_stale_sibling() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "stale-root",
            None,
            None,
            Some("compression"),
            Some("2026-06-03T00:00:30Z"),
            "2026-06-03T00:00:00Z",
        );
        insert_session(
            &conn,
            "stale-real",
            Some("stale-root"),
            None,
            None,
            None,
            "2026-06-03T00:00:10Z",
        );
        insert_message_at(
            &conn,
            "stale-real",
            "user",
            "live continuation",
            "2026-06-03T00:00:40Z",
        );
        insert_session(
            &conn,
            "stale-closed",
            Some("stale-root"),
            None,
            None,
            Some("2026-06-03T00:00:50Z"),
            "2026-06-03T00:00:45Z",
        );
        insert_message_at(
            &conn,
            "stale-closed",
            "user",
            "stale closed sibling with later message",
            "2026-06-03T00:00:55Z",
        );

        let output = backend
            .search(Some("stale-root"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["stale-real".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_excludes_branch_delegate_and_tool_children_from_tip() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "exclude-root",
            None,
            None,
            Some("compression"),
            Some("2026-06-03T00:00:10Z"),
            "2026-06-03T00:00:00Z",
        );
        insert_session(
            &conn,
            "exclude-cont",
            Some("exclude-root"),
            None,
            None,
            None,
            "2026-06-03T00:00:20Z",
        );
        insert_message_at(
            &conn,
            "exclude-cont",
            "user",
            "real continuation",
            "2026-06-03T00:01:00Z",
        );
        for (id, config, source, message_at) in [
            (
                "exclude-branch",
                Some(r#"{"_branched_from":"exclude-root"}"#),
                "cli",
                "2026-06-03T00:02:00Z",
            ),
            (
                "exclude-delegate",
                Some(r#"{"_delegate_from":"exclude-root"}"#),
                "cli",
                "2026-06-03T00:03:00Z",
            ),
            ("exclude-tool", None, "tool", "2026-06-03T00:04:00Z"),
        ] {
            insert_session(
                &conn,
                id,
                Some("exclude-root"),
                None,
                None,
                None,
                message_at,
            );
            if let Some(config) = config {
                update_session_model_config(&conn, id, config);
            }
            update_session_platform(&conn, id, source);
            insert_message_at(&conn, id, "user", "must not become tip", message_at);
        }

        let output = backend
            .search(Some("exclude-root"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["exclude-cont".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_treats_like_wildcards_literally() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "literal%id",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(&conn, "literal%id", "user", "literal percent id");
        insert_session(
            &conn,
            "literalXid",
            None,
            None,
            None,
            None,
            "2026-06-03T09:03:00Z",
        );
        insert_message(&conn, "literalXid", "user", "wildcard should not match");

        let output = backend
            .search(Some("%"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["literal%id".to_string()], "{output}");
    }
}
