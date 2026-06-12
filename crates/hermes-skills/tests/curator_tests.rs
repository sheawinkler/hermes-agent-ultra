//! Integration tests for the curator engine (hermes_skills::curator).
//!
//! Placed here as an independent compilation unit to avoid interference
//! from pre-existing `#[cfg(test)]` issues in sibling modules.
//!
//! ## Coverage
//!
//! | Section | Tests | What it validates |
//! |---------|-------|-------------------|
//! | CuratorState IO | 5 | load/save roundtrip, missing file default, corrupt JSON recovery, atomic write integrity |
//! | Scheduling gate | 5 | should_run_now: enabled/disabled, paused, first-run seeding, interval expiry |
//! | Pause / Resume | 3 | set_paused, is_paused, state roundtrip |
//! | Auto transitions | 7 | archive/stale/reactivation rules, pinned skip, no-activity skip, edge cases |
//! | Report writing | 3 | write_curator_report produces run.json + REPORT.md, timestamped directory |
//! | Prompt building | 2 | build_curator_prompt produces expected table structure |
//! | Config serde | 2 | CuratorConfig serialization roundtrip with defaults |
//!
//! ## How to run
//!
//! ```bash
//! cargo test -p hermes-skills --test curator_tests
//! cargo test -p hermes-skills --test curator_tests -- --nocapture
//! cargo test -p hermes-skills --test curator_tests curator_state -- --nocapture
//! ```

use std::fs;

use chrono::Utc;
use hermes_skills::{
    UsageStore, CuratorConfig, CuratorRunCounts, CuratorRunReport, CuratorState, TransitionResult,
    apply_automatic_transitions, build_curator_prompt, is_paused, load_curator_state,
    maybe_run_curator, save_curator_state, set_paused, should_run_now, write_curator_report,
    SkillUsageRecord, STATE_ACTIVE, STATE_ARCHIVED, STATE_STALE,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a temporary skills directory with a `.usage.json` containing the
/// given records. Returns the dir handle (which cleans up on drop) and a
/// UsageStore rooted at the temp dir.
fn setup_skills_dir(records: Vec<(&str, SkillUsageRecord)>) -> (tempfile::TempDir, UsageStore) {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let usage: std::collections::BTreeMap<String, SkillUsageRecord> = records
        .into_iter()
        .map(|(name, rec)| (name.to_string(), rec))
        .collect();
    store.save_usage(&usage).expect("save_usage");
    (dir, store)
}

/// Create a SkillUsageRecord with given state and optional activity timestamp.
fn make_record(state: &str, last_used_at: Option<&str>, pinned: bool) -> SkillUsageRecord {
    SkillUsageRecord {
        state: state.to_string(),
        pinned,
        last_used_at: last_used_at.map(|s| s.to_string()),
        last_viewed_at: None,
        last_patched_at: None,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. CuratorState IO
// ---------------------------------------------------------------------------

#[test]
fn curator_state_default_on_missing_file() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let state = load_curator_state(&store);
    assert!(!state.paused);
    assert_eq!(state.run_count, 0);
    assert!(state.last_run_at.is_none());
}

#[test]
fn curator_state_roundtrip_save_load() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let mut expected = CuratorState::default();
    expected.paused = true;
    expected.run_count = 42;
    expected.last_run_at = Some("2025-06-01T12:00:00+00:00".to_string());
    expected.last_run_summary = Some("test summary".to_string());
    expected.last_report_path = Some("/tmp/report".to_string());

    save_curator_state(&store, &expected).expect("save");
    let loaded = load_curator_state(&store);
    assert_eq!(loaded.paused, true);
    assert_eq!(loaded.run_count, 42);
    assert_eq!(loaded.last_run_at, expected.last_run_at);
    assert_eq!(loaded.last_run_summary, expected.last_run_summary);
    assert_eq!(loaded.last_report_path, expected.last_report_path);
}

#[test]
fn curator_state_corrupt_json_recovers() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let state_path = dir.path().join(".curator_state");
    fs::write(&state_path, "{not valid json!!!").expect("write");

    let state = load_curator_state(&store);
    // Should fall back to default
    assert!(!state.paused);
    assert_eq!(state.run_count, 0);
}

#[test]
fn curator_state_persists_empty_state() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let state = CuratorState::default();
    save_curator_state(&store, &state).expect("save");
    assert!(dir.path().join(".curator_state").exists());
    let loaded = load_curator_state(&store);
    assert!(!loaded.paused);
}

#[test]
fn curator_state_atomic_write_no_corruption() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let state = CuratorState::default();
    save_curator_state(&store, &state).expect("save");

    // Verify no .tmp files left behind
    let tmp_files: Vec<_> = fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.ends_with(".tmp"))
                .unwrap_or(false)
        })
        .collect();
    assert!(tmp_files.is_empty(), "tmp files left after atomic write");
}

// ---------------------------------------------------------------------------
// 2. Scheduling gate
// ---------------------------------------------------------------------------

#[test]
fn should_run_now_disabled_returns_false() {
    let (_dir, store) = setup_skills_dir(vec![]);
    let config = CuratorConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(!should_run_now(&store, &config));
}

#[test]
fn should_run_now_paused_returns_false() {
    let (_dir, store) = setup_skills_dir(vec![]);
    let config = CuratorConfig {
        enabled: true,
        ..Default::default()
    };
    set_paused(&store, true).expect("set_paused");
    assert!(!should_run_now(&store, &config));
}

#[test]
fn should_run_now_first_run_seeds_and_defers() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let config = CuratorConfig {
        enabled: true,
        interval_hours: 168,
        ..Default::default()
    };
    // No state file → first run: should seed and return false
    assert!(!should_run_now(&store, &config));
    // State file should now exist with last_run_at seeded
    let state = load_curator_state(&store);
    assert!(state.last_run_at.is_some(), "first run should seed last_run_at");
}

#[test]
fn should_run_now_within_interval_returns_false() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let config = CuratorConfig {
        enabled: true,
        interval_hours: 24,
        ..Default::default()
    };

    // Simulate just-now run
    let mut state = CuratorState::default();
    state.last_run_at = Some(Utc::now().to_rfc3339());
    save_curator_state(&store, &state).expect("save");

    assert!(!should_run_now(&store, &config), "just ran, should not run again");
}

#[test]
fn should_run_now_after_interval_returns_true() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    let config = CuratorConfig {
        enabled: true,
        interval_hours: 1, // 1 hour
        ..Default::default()
    };

    // Simulate last run 2 hours ago
    let mut state = CuratorState::default();
    let past = Utc::now() - chrono::Duration::seconds(7200);
    state.last_run_at = Some(past.to_rfc3339());
    save_curator_state(&store, &state).expect("save");

    assert!(should_run_now(&store, &config), "interval elapsed, should run");
}

// ---------------------------------------------------------------------------
// 3. Pause / Resume
// ---------------------------------------------------------------------------

#[test]
fn pause_resume_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());

    assert!(!is_paused(&store), "initially not paused");

    set_paused(&store, true).expect("set_paused");
    assert!(is_paused(&store), "paused after set_paused(true)");

    set_paused(&store, false).expect("set_paused");
    assert!(!is_paused(&store), "not paused after set_paused(false)");
}

#[test]
fn pause_state_persists_across_loads() {
    let dir = tempdir().expect("tempdir");
    let store = UsageStore::with_dir(dir.path().to_path_buf());
    set_paused(&store, true).expect("set_paused");

    let state = load_curator_state(&store);
    assert!(state.paused);
}

#[test]
fn maybe_run_curator_insufficient_idle_returns_false() {
    let (_dir, store) = setup_skills_dir(vec![]);
    let config = CuratorConfig {
        enabled: true,
        min_idle_hours: 2,
        interval_hours: 1,
        ..Default::default()
    };

    // Set up a "just ran" state
    let mut state = CuratorState::default();
    let past = Utc::now() - chrono::Duration::seconds(7200);
    state.last_run_at = Some(past.to_rfc3339());
    save_curator_state(&store, &state).unwrap();

    // Only 30 seconds idle
    assert!(!maybe_run_curator(&store, &config, 30));
}

// ---------------------------------------------------------------------------
// 4. Automatic transitions
// ---------------------------------------------------------------------------

#[test]
fn auto_transitions_no_records_returns_all_zero() {
    let (_dir, store) = setup_skills_dir(vec![]);
    let config = CuratorConfig::default();
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.checked, 0);
    assert_eq!(result.marked_stale, 0);
    assert_eq!(result.archived, 0);
    assert_eq!(result.reactivated, 0);
}

#[test]
fn auto_transitions_pinned_skill_is_skipped() {
    let now = Utc::now();
    let old = (now - chrono::Duration::seconds(86400 * 60)).to_rfc3339(); // 60 days ago
    let (_dir, store) = setup_skills_dir(vec![(
        "pinned_skill",
        make_record(STATE_ACTIVE, Some(&old), true), // pinned!
    )]);
    let config = CuratorConfig {
        stale_after_days: 30,
        archive_after_days: 90,
        ..Default::default()
    };
    let result = apply_automatic_transitions(&store, &config);
    // Pinned skills are skipped entirely — not even checked
    assert_eq!(result.checked, 0);
}

#[test]
fn auto_transitions_active_to_stale() {
    let now = Utc::now();
    let old = (now - chrono::Duration::seconds(86400 * 60)).to_rfc3339(); // 60 days → past stale threshold
    let (_dir, store) = setup_skills_dir(vec![(
        "stale_skill",
        make_record(STATE_ACTIVE, Some(&old), false),
    )]);
    let config = CuratorConfig {
        stale_after_days: 30,
        archive_after_days: 90,
        ..Default::default()
    };
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.checked, 1);
    assert_eq!(result.marked_stale, 1);

    // Verify the state actually changed in .usage.json
    let usage = store.load_usage();
    assert_eq!(usage.get("stale_skill").unwrap().state, STATE_STALE);
}

#[test]
fn auto_transitions_active_to_archived() {
    let now = Utc::now();
    let ancient = (now - chrono::Duration::seconds(86400 * 100)).to_rfc3339(); // 100 days → past archive
    let (_dir, store) = setup_skills_dir(vec![(
        "old_skill",
        make_record(STATE_ACTIVE, Some(&ancient), false),
    )]);
    let config = CuratorConfig {
        stale_after_days: 30,
        archive_after_days: 90,
        ..Default::default()
    };
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.archived, 1);
    assert_eq!(result.marked_stale, 0); // archive takes priority
    let usage = store.load_usage();
    assert_eq!(usage.get("old_skill").unwrap().state, STATE_ARCHIVED);
}

#[test]
fn auto_transitions_reactivation_stale_to_active() {
    let now = Utc::now();
    let recent = (now - chrono::Duration::seconds(86400 * 5)).to_rfc3339(); // 5 days → within stale window
    let (_dir, store) = setup_skills_dir(vec![(
        "revived_skill",
        make_record(STATE_STALE, Some(&recent), false),
    )]);
    let config = CuratorConfig {
        stale_after_days: 30,
        archive_after_days: 90,
        ..Default::default()
    };
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.reactivated, 1);
    let usage = store.load_usage();
    assert_eq!(usage.get("revived_skill").unwrap().state, STATE_ACTIVE);
}

#[test]
fn auto_transitions_skill_with_no_activity_is_skipped() {
    // No last_used_at / last_viewed_at / last_patched_at
    let (_dir, store) = setup_skills_dir(vec![(
        "never_touched",
        make_record(STATE_ACTIVE, None, false),
    )]);
    let config = CuratorConfig::default();
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.checked, 0);
}

#[test]
fn auto_transitions_batch_mixed_state() {
    let now = Utc::now();
    let old = (now - chrono::Duration::seconds(86400 * 60)).to_rfc3339();
    let ancient = (now - chrono::Duration::seconds(86400 * 100)).to_rfc3339();
    let recent = (now - chrono::Duration::seconds(86400 * 5)).to_rfc3339();

    let (_dir, store) = setup_skills_dir(vec![
        ("pinned_old", make_record(STATE_ACTIVE, Some(&old), true)),      // pinned → skipped
        ("going_stale", make_record(STATE_ACTIVE, Some(&old), false)),    // 60d → stale
        ("going_archive", make_record(STATE_ACTIVE, Some(&ancient), false)), // 100d → archive
        ("reviving", make_record(STATE_STALE, Some(&recent), false)),     // 5d → reactivate
        ("active_fresh", make_record(STATE_ACTIVE, Some(&recent), false)), // fresh → nothing
    ]);
    let config = CuratorConfig {
        stale_after_days: 30,
        archive_after_days: 90,
        ..Default::default()
    };
    let result = apply_automatic_transitions(&store, &config);
    assert_eq!(result.checked, 4); // pinned skipped
    assert_eq!(result.marked_stale, 1); // going_stale
    assert_eq!(result.archived, 1); // going_archive
    assert_eq!(result.reactivated, 1); // reviving
}

// ---------------------------------------------------------------------------
// 5. Report writing
// ---------------------------------------------------------------------------

#[test]
fn write_curator_report_creates_run_json_and_report_md() {
    let dir = tempdir().expect("tempdir");
    let logs_dir = dir.path().join("logs");

    let report = CuratorRunReport {
        started_at: Utc::now().to_rfc3339(),
        duration_seconds: 12.5,
        model: Some("gpt-5".to_string()),
        provider: Some("openai".to_string()),
        dry_run: false,
        auto_transitions: TransitionResult {
            checked: 10,
            marked_stale: 3,
            archived: 1,
            reactivated: 0,
        },
        counts: CuratorRunCounts {
            before: 50,
            after: 48,
            delta: -2,
            state_transitions: 4,
            ..Default::default()
        },
        consolidated: vec![],
        pruned: vec![],
        tool_calls: vec![],
        llm_error: None,
    };

    let report_path = write_curator_report(&report, &logs_dir).expect("write report");
    assert!(report_path.starts_with(&logs_dir.join("curator")));
    assert!(report_path.join("run.json").exists());
    assert!(report_path.join("REPORT.md").exists());

    // Verify run.json is valid JSON
    let json_content = fs::read_to_string(report_path.join("run.json")).expect("read run.json");
    let _parsed: serde_json::Value = serde_json::from_str(&json_content).expect("valid json");
}

#[test]
fn write_curator_report_dry_run_creates_report() {
    let dir = tempdir().expect("tempdir");
    let report = CuratorRunReport {
        started_at: Utc::now().to_rfc3339(),
        duration_seconds: 0.5,
        model: None,
        provider: None,
        dry_run: true,
        auto_transitions: TransitionResult::default(),
        counts: Default::default(),
        consolidated: vec![],
        pruned: vec![],
        tool_calls: vec![],
        llm_error: None,
    };
    let report_path = write_curator_report(&report, dir.path()).expect("write");
    assert!(report_path.join("REPORT.md").exists());
}

#[test]
fn write_curator_report_with_llm_error() {
    let dir = tempdir().expect("tempdir");
    let report = CuratorRunReport {
        started_at: Utc::now().to_rfc3339(),
        duration_seconds: 0.1,
        model: Some("gpt-5".to_string()),
        provider: Some("openai".to_string()),
        dry_run: false,
        auto_transitions: TransitionResult::default(),
        counts: Default::default(),
        consolidated: vec![],
        pruned: vec![],
        tool_calls: vec![],
        llm_error: Some("API rate limit exceeded".to_string()),
    };
    let path = write_curator_report(&report, dir.path()).expect("write");
    let md = fs::read_to_string(path.join("REPORT.md")).expect("read");
    assert!(md.contains("API rate limit exceeded"));
}

// ---------------------------------------------------------------------------
// 6. Prompt building
// ---------------------------------------------------------------------------

#[test]
fn build_curator_prompt_with_no_skills_has_header() {
    let (_dir, store) = setup_skills_dir(vec![]);
    let prompt = build_curator_prompt(&store);
    // The prompt starts with the curator role preamble
    assert!(prompt.contains("Hermes' background skill CURATOR"));
    // Even with no agent-created skills, the table header should appear
    assert!(prompt.contains("Current skill inventory"));
}

#[test]
fn build_curator_prompt_with_skills_includes_table() {
    let now = Utc::now();
    let (_dir, store) = setup_skills_dir(vec![
        (
            "skill_a",
            SkillUsageRecord {
                state: STATE_ACTIVE.to_string(),
                use_count: 10,
                agent_created: true, // required for agent_created_report()
                last_used_at: Some(now.to_rfc3339()),
                ..Default::default()
            },
        ),
        (
            "skill_b",
            SkillUsageRecord {
                state: STATE_STALE.to_string(),
                use_count: 1,
                agent_created: true,
                last_used_at: Some(now.to_rfc3339()),
                ..Default::default()
            },
        ),
    ]);
    let prompt = build_curator_prompt(&store);
    // Both skills should appear in the markdown table
    assert!(prompt.contains("skill_a"), "prompt should mention skill_a");
    assert!(prompt.contains("skill_b"), "prompt should mention skill_b");
    // Table has proper markdown formatting
    assert!(prompt.contains("| Name | State | Pinned | Activity | Last Active |"));
}

// ---------------------------------------------------------------------------
// 7. Config serde
// ---------------------------------------------------------------------------

#[test]
fn curator_config_default_values_match_spec() {
    let config = CuratorConfig::default();
    assert!(config.enabled, "default enabled should be true");
    assert_eq!(config.interval_hours, 168);
    assert_eq!(config.min_idle_hours, 2);
    assert_eq!(config.stale_after_days, 30);
    assert_eq!(config.archive_after_days, 90);
    assert!(config.prune_builtins, "default prune_builtins should be true");
}

#[test]
fn curator_config_json_roundtrip() {
    let config = CuratorConfig {
        enabled: false,
        interval_hours: 24,
        min_idle_hours: 1,
        stale_after_days: 15,
        archive_after_days: 60,
        prune_builtins: false,
    };
    let json = serde_json::to_string_pretty(&config).expect("serialize");
    let roundtripped: CuratorConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(config, roundtripped);
}
