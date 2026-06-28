use super::*;
use hermes_core::MessageRole;

fn make_test_trajectory() -> Trajectory {
    let now = Utc::now();
    Trajectory {
        id: "test-001".to_string(),
        prompt: "Write a hello world program".to_string(),
        messages: vec![
            Message::system("You are a helpful assistant"),
            Message {
                role: MessageRole::User,
                content: Some("Write a hello world program".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: Some("Let me use a tool".to_string()),
                tool_calls: Some(vec![ToolCall {
                    id: "tc-1".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "run_code".to_string(),
                        arguments: "{}".to_string(),
                    },
                    extra_content: None,
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
            Message {
                role: MessageRole::Tool,
                content: Some("output: Hello, World!".to_string()),
                tool_calls: None,
                tool_call_id: Some("tc-1".to_string()),
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: Some("Here is your program".to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            },
        ],
        tool_calls: vec![ToolCall {
            id: "tc-1".to_string(),
            function: hermes_core::FunctionCall {
                name: "run_code".to_string(),
                arguments: "{}".to_string(),
            },
            extra_content: None,
        }],
        outcome: TrajectoryOutcome::Success,
        reward: Some(1.0),
        timestamp: now,
    }
}

#[test]
fn test_trajectory_outcome_serde() {
    let outcome = TrajectoryOutcome::Success;
    let json = serde_json::to_string(&outcome).unwrap();
    assert_eq!(json, "\"success\"");
    let de: TrajectoryOutcome = serde_json::from_str(&json).unwrap();
    assert_eq!(de, outcome);
}

#[test]
fn test_trajectory_serde_roundtrip() {
    let t = make_test_trajectory();
    let json = serde_json::to_string(&t).unwrap();
    let de: Trajectory = serde_json::from_str(&json).unwrap();
    assert_eq!(de.id, t.id);
    assert_eq!(de.prompt, t.prompt);
    assert_eq!(de.outcome, t.outcome);
    assert_eq!(de.reward, t.reward);
}

#[test]
fn test_compressor_keeps_first_last_and_tool_call_messages() {
    let t = make_test_trajectory();
    // 5 messages: system, user, assistant (tool_calls), tool, assistant
    assert_eq!(t.messages.len(), 5);

    let compressed = TrajectoryCompressor::new().compress(&t);
    // Should keep: index 0 (first), 2 (has tool_calls), 4 (last) = 3 messages
    assert_eq!(compressed.messages.len(), 3);
    assert_eq!(compressed.messages[0].role, MessageRole::System);
    // Index 2 had tool calls
    assert!(compressed.messages[1].tool_calls.is_some());
    assert_eq!(compressed.messages[2].role, MessageRole::Assistant);
}

#[test]
fn test_compressor_short_trajectory_unchanged() {
    let t = Trajectory {
        id: "short".to_string(),
        prompt: "hi".to_string(),
        messages: vec![Message::system("hello")],
        tool_calls: vec![],
        outcome: TrajectoryOutcome::Timeout,
        reward: None,
        timestamp: Utc::now(),
    };
    let compressed = TrajectoryCompressor::new().compress(&t);
    assert_eq!(compressed.messages.len(), 1);
}

#[test]
fn test_batch_generator_trajectories() {
    let gen = BatchGenerator::new();
    let config = BatchConfig::default();
    let result = gen.generate_batch(vec!["prompt1".to_string()], &config);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].messages.len(), 2);
    assert_eq!(result[0].outcome, TrajectoryOutcome::Success);
    assert!(result[0]
        .messages
        .get(1)
        .and_then(|m| m.content.as_deref())
        .unwrap_or("")
        .contains("Baseline rollout generated"));
}

#[test]
fn test_rl_toolset_lifecycle() {
    let ts = RlToolset::new();
    assert!(!ts.list_environments().is_empty());
    assert_eq!(
        ts.configure_environment("test", &serde_json::Value::Null),
        "configured"
    );
    let id = ts.start_training("test");
    assert!(id.starts_with("rl-session-"));
    let running = ts.get_results(&id);
    assert_eq!(running["status"], "running");
    assert_eq!(ts.stop_training(&id), "stopped");
    let stopped = ts.get_results(&id);
    assert_eq!(stopped["status"], "stopped");
}

#[test]
fn test_batch_config_default() {
    let config = BatchConfig::default();
    assert_eq!(config.max_trajectories, 32);
    assert_eq!(config.max_turns_per_trajectory, 10);
    assert_eq!(config.model, "gpt-4o");
    assert!((config.temperature - 0.7).abs() < f64::EPSILON);
}

#[test]
fn training_status_serde_uses_snake_case() {
    let json = serde_json::to_string(&TrainingStatus::Running).unwrap();
    assert_eq!(json, "\"running\"");
    let parsed: TrainingStatus = serde_json::from_str("\"stopped\"").unwrap();
    assert_eq!(parsed, TrainingStatus::Stopped);
}

#[test]
fn run_manager_tracks_status_metrics_and_sorted_runs() {
    let mut manager = RunManager::new(PathBuf::from("/tmp/hermes-rl-test"));
    let config = TrainingConfig {
        max_steps: 42,
        ..TrainingConfig::default()
    };
    let run_id = manager.create_run("tinker", config.clone());

    let run = manager.get_run(&run_id).unwrap();
    assert_eq!(run.environment, "tinker");
    assert_eq!(run.status, TrainingStatus::Pending);
    assert_eq!(run.metrics.total_steps, 42);
    assert_eq!(run.config, config);

    assert!(manager.set_status(&run_id, TrainingStatus::Running));
    assert!(manager.update_metrics(
        &run_id,
        TrainingMetrics {
            total_steps: 42,
            current_step: 7,
            reward_mean: Some(0.25),
            reward_std: Some(0.5),
            loss: Some(0.75),
        },
    ));
    let run = manager.get_run(&run_id).unwrap();
    assert_eq!(run.status, TrainingStatus::Running);
    assert_eq!(run.metrics.current_step, 7);

    assert!(manager.set_status(&run_id, TrainingStatus::Stopped));
    let run = manager.get_run(&run_id).unwrap();
    assert_eq!(run.status, TrainingStatus::Stopped);
    assert!(run.finished_at.is_some());
    assert_eq!(manager.list_runs().len(), 1);
}

#[test]
fn rl_environments_expose_tinker_atropos_and_custom() {
    let envs = RlEnvironment::builtin_environments();
    let names: Vec<_> = envs.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"tinker"));
    assert!(names.contains(&"atropos"));
    assert!(names.contains(&"custom"));
    assert!(envs
        .iter()
        .all(|e| e.config_schema.get("type").and_then(|v| v.as_str()) == Some("object")));
}

#[test]
fn batch_runner_generates_style_tagged_baseline_responses() {
    let runner = BatchRunner::new(BatchRunnerConfig {
        max_parallel_jobs: 2,
        max_turns: 5,
    });
    let prompts = vec!["Fix the flaky test and verify it".to_string()];
    let trajectories = runner.generate_batch(&prompts);

    assert_eq!(trajectories.len(), 1);
    assert_eq!(trajectories[0].id, "traj-1");
    assert_eq!(trajectories[0].prompt, prompts[0]);
    assert!(trajectories[0].response.contains("[baseline-diagnostic]"));
    assert!(trajectories[0].response.contains("steps_budget=5"));
}

#[test]
fn parse_batch_jsonl_dataset_accepts_prompt_and_conversation_rows() {
    let raw = r#"
{"prompt":" Plan a verification slice ","source":"direct"}
{"conversations":[{"role":"system","content":"ignore"},{"role":"user","content":"Fix the bug"}],"difficulty":"medium"}
{"conversations":[{"from":"assistant","value":"hello"},{"from":"human","value":"Write tests"}]}
{"conversations":[{"role":"assistant","content":"no user prompt"}]}

"#;

    let items = parse_batch_jsonl_dataset(raw).unwrap();

    assert_eq!(items.len(), 3);
    assert_eq!(items[0].prompt_index, 0);
    assert_eq!(items[0].prompt, "Plan a verification slice");
    assert_eq!(items[0].metadata["source"], "direct");
    assert_eq!(items[1].prompt, "Fix the bug");
    assert_eq!(items[1].metadata["difficulty"], "medium");
    assert_eq!(items[2].prompt, "Write tests");
    assert!(items[1].metadata.get("conversations").is_none());
}

#[test]
fn parse_batch_jsonl_dataset_reports_invalid_or_empty_inputs() {
    let invalid = parse_batch_jsonl_dataset("{not json").unwrap_err();
    assert!(invalid.contains("line 1: invalid JSON"));

    let empty = parse_batch_jsonl_dataset(r#"{"prompt":"   "}"#).unwrap_err();
    assert_eq!(empty, "no valid batch dataset prompts found");
}

#[test]
fn batch_dataset_runner_updates_checkpoint_and_resumes_by_index_and_prompt() {
    let dataset = vec![
        BatchDatasetItem {
            prompt_index: 0,
            prompt: "Fix the parser".to_string(),
            metadata: Map::new(),
        },
        BatchDatasetItem {
            prompt_index: 1,
            prompt: "Plan the rollout".to_string(),
            metadata: Map::new(),
        },
        BatchDatasetItem {
            prompt_index: 2,
            prompt: "Verify the behavior".to_string(),
            metadata: Map::new(),
        },
    ];
    let mut prior = BatchRunCheckpoint::new("resume-run");
    prior.completed_prompts = vec![0];
    prior.completed_prompt_texts = vec!["Plan the rollout".to_string()];
    let config = BatchDatasetRunConfig {
        run_name: "resume-run".to_string(),
        batch_size: 2,
        selected_toolsets: vec!["rl_training".to_string()],
        ..BatchDatasetRunConfig::default()
    };

    let report = BatchDatasetRunner::new().run(&dataset, &config, Some(prior));

    assert_eq!(report.statistics.total_prompts, 3);
    assert_eq!(report.statistics.total_batches, 2);
    assert_eq!(report.statistics.processed, 1);
    assert_eq!(report.statistics.skipped, 2);
    assert_eq!(report.trajectories.len(), 1);
    assert_eq!(report.trajectories[0].prompt_index, 2);
    assert_eq!(report.trajectories[0].toolsets_used, ["rl_training"]);
    assert_eq!(report.checkpoint.completed_prompts, [0, 1, 2]);
    assert!(report
        .checkpoint
        .completed_prompt_texts
        .contains(&"Verify the behavior".to_string()));
    assert_eq!(
        report.checkpoint.batch_stats.get("0").unwrap(),
        &BatchCheckpointStats {
            processed: 0,
            skipped: 2,
        }
    );
    assert_eq!(
        report.checkpoint.batch_stats.get("1").unwrap(),
        &BatchCheckpointStats {
            processed: 1,
            skipped: 0,
        }
    );
}

#[test]
fn batch_dataset_runner_omits_ephemeral_system_prompt_from_outputs() {
    let dataset = vec![BatchDatasetItem {
        prompt_index: 0,
        prompt: "Test prompt leakage".to_string(),
        metadata: Map::new(),
    }];
    let config = BatchDatasetRunConfig {
        run_name: "ephemeral-run".to_string(),
        ephemeral_system_prompt: Some("never persist this system prompt".to_string()),
        ..BatchDatasetRunConfig::default()
    };

    let report = BatchDatasetRunner::new().run(&dataset, &config, None);
    let jsonl = report.trajectories_jsonl().unwrap();
    let checkpoint = report.checkpoint.to_json_pretty().unwrap();

    assert!(report.statistics.ephemeral_system_prompt_used);
    assert!(!jsonl.contains("never persist this system prompt"));
    assert!(!checkpoint.contains("never persist this system prompt"));
    assert_eq!(
        report.trajectories[0].conversations[0].content.as_deref(),
        Some("Test prompt leakage")
    );
}

#[test]
fn batch_dataset_runner_respects_max_samples_and_nonzero_batch_size() {
    let dataset: Vec<_> = (0..5)
        .map(|idx| BatchDatasetItem {
            prompt_index: idx,
            prompt: format!("prompt {idx}"),
            metadata: Map::new(),
        })
        .collect();
    let config = BatchDatasetRunConfig {
        run_name: "limited-run".to_string(),
        batch_size: 0,
        max_samples: Some(3),
        ..BatchDatasetRunConfig::default()
    };

    let report = BatchDatasetRunner::new().run(&dataset, &config, None);

    assert_eq!(report.statistics.batch_size, 1);
    assert_eq!(report.statistics.total_prompts, 3);
    assert_eq!(report.statistics.total_batches, 3);
    assert_eq!(report.statistics.processed, 3);
    assert_eq!(report.checkpoint.completed_prompts, [0, 1, 2]);
    assert_eq!(report.trajectories.len(), 3);
}

#[test]
fn atomic_json_write_replaces_without_temp_file_leaks() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested").join("checkpoint.json");

    atomic_json_write(&path, &serde_json::json!({"version": 1})).unwrap();
    atomic_json_write(&path, &serde_json::json!({"version": 2, "ok": true})).unwrap();

    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["ok"], true);

    let leaked_tmp_files: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leaked_tmp_files.is_empty());
}

#[test]
fn atomic_json_write_preserves_original_on_serialization_error() {
    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("intentional test failure"))
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("checkpoint.json");
    atomic_json_write(&path, &serde_json::json!({"preserved": true})).unwrap();

    let err = atomic_json_write(&path, &FailingSerialize).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value, serde_json::json!({"preserved": true}));
    let leaked_tmp_files: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leaked_tmp_files.is_empty());
}

#[test]
fn atomic_json_write_handles_unicode_and_concurrent_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = std::sync::Arc::new(tmp.path().join("concurrent.json"));
    let mut handles = Vec::new();

    for writer in 0..8 {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let japanese = "\u{65e5}\u{672c}\u{8a9e}";
            atomic_json_write(
                &*path,
                &serde_json::json!({
                    "writer": writer,
                    "emoji": "sparkles",
                    "japanese": japanese,
                    "data": (0..32).collect::<Vec<_>>()
                }),
            )
        }));
    }

    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let value: Value = serde_json::from_str(&std::fs::read_to_string(&*path).unwrap()).unwrap();
    assert!(value["writer"].as_u64().unwrap() < 8);
    assert_eq!(value["japanese"], "\u{65e5}\u{672c}\u{8a9e}");
    assert_eq!(value["data"].as_array().unwrap().len(), 32);
}

#[test]
fn batch_run_checkpoint_load_handles_missing_corrupt_and_mismatched_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("checkpoint.json");

    let missing = BatchRunCheckpoint::load_from_path(&path).unwrap();
    assert!(missing.is_none());

    std::fs::write(&path, "{broken json").unwrap();
    let strict_err = BatchRunCheckpoint::load_from_path(&path).unwrap_err();
    assert_eq!(strict_err.kind(), io::ErrorKind::InvalidData);
    let recovered = BatchRunCheckpoint::load_or_new_for_run(&path, "fresh-run").unwrap();
    assert_eq!(recovered.run_name, "fresh-run");
    assert!(recovered.completed_prompts.is_empty());

    let mut prior = BatchRunCheckpoint::new("old-run");
    prior.completed_prompts = vec![7, 8, 9];
    prior.save_atomic_to_path(&path).unwrap();
    let fresh = BatchRunCheckpoint::load_or_new_for_run(&path, "new-run").unwrap();
    assert_eq!(fresh.run_name, "new-run");
    assert!(fresh.completed_prompts.is_empty());

    let existing = BatchRunCheckpoint::load_or_new_for_run(&path, "old-run").unwrap();
    assert_eq!(existing.completed_prompts, [7, 8, 9]);
}

#[test]
fn batch_dataset_runner_persists_atomic_checkpoint_per_batch_and_resumes_from_file() {
    let tmp = tempfile::tempdir().unwrap();
    let checkpoint_path = tmp.path().join("checkpoints").join("run.json");
    let dataset: Vec<_> = (0..4)
        .map(|idx| BatchDatasetItem {
            prompt_index: idx,
            prompt: format!("prompt {idx}"),
            metadata: Map::new(),
        })
        .collect();
    let config = BatchDatasetRunConfig {
        run_name: "atomic-run".to_string(),
        batch_size: 2,
        ..BatchDatasetRunConfig::default()
    };

    let first = BatchDatasetRunner::new()
        .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
        .unwrap();
    assert_eq!(first.statistics.processed, 4);
    assert_eq!(first.statistics.skipped, 0);
    assert_eq!(first.statistics.checkpoints_written, 2);
    assert_eq!(first.checkpoint.completed_prompts, [0, 1, 2, 3]);

    let persisted = BatchRunCheckpoint::load_from_path(&checkpoint_path)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.run_name, "atomic-run");
    assert!(persisted.last_updated.is_some());
    assert_eq!(persisted.completed_prompts, [0, 1, 2, 3]);
    assert_eq!(
        persisted.batch_stats.get("0").unwrap(),
        &BatchCheckpointStats {
            processed: 2,
            skipped: 0,
        }
    );
    assert_eq!(
        persisted.batch_stats.get("1").unwrap(),
        &BatchCheckpointStats {
            processed: 2,
            skipped: 0,
        }
    );

    let resumed = BatchDatasetRunner::new()
        .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
        .unwrap();
    assert_eq!(resumed.statistics.processed, 0);
    assert_eq!(resumed.statistics.skipped, 4);
    assert_eq!(resumed.statistics.checkpoints_written, 2);
    assert!(resumed.trajectories.is_empty());
    assert_eq!(resumed.checkpoint.completed_prompts, [0, 1, 2, 3]);
}

#[test]
fn batch_dataset_runner_checkpoint_path_starts_fresh_on_corrupt_or_mismatched_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let checkpoint_path = tmp.path().join("checkpoint.json");
    let dataset = vec![BatchDatasetItem {
        prompt_index: 0,
        prompt: "recover from checkpoint".to_string(),
        metadata: Map::new(),
    }];
    let config = BatchDatasetRunConfig {
        run_name: "fresh-run".to_string(),
        batch_size: 1,
        ..BatchDatasetRunConfig::default()
    };

    std::fs::write(&checkpoint_path, "{broken json").unwrap();
    let recovered = BatchDatasetRunner::new()
        .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
        .unwrap();
    assert_eq!(recovered.statistics.processed, 1);
    assert_eq!(recovered.checkpoint.run_name, "fresh-run");

    let mut wrong_run = BatchRunCheckpoint::new("other-run");
    wrong_run.completed_prompts = vec![0];
    wrong_run.save_atomic_to_path(&checkpoint_path).unwrap();
    let mismatched = BatchDatasetRunner::new()
        .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
        .unwrap();
    assert_eq!(mismatched.statistics.processed, 1);
    assert_eq!(mismatched.statistics.skipped, 0);
    assert_eq!(mismatched.checkpoint.run_name, "fresh-run");
}
