fn render_kanban_status(board: &KanbanBoard) -> String {
    let mut out = String::new();
    let _ = writeln!(
        &mut out,
        "Kanban board: {} ({})",
        board.name.trim(),
        board.id.trim()
    );
    if let Some(project_path) = board.project_path.as_deref() {
        let _ = writeln!(&mut out, "Project: {}", project_path);
    }
    let counts = lane_counts(board);
    let total: usize = counts.iter().map(|(_, count)| *count).sum();
    let _ = writeln!(
        &mut out,
        "Tasks: {} (archived done: {})",
        total,
        board.archived.len()
    );
    for (lane, count) in counts {
        let _ = writeln!(&mut out, "  {:>7}: {}", lane.as_str(), count);
    }
    if board.tasks.is_empty() {
        let _ = writeln!(&mut out, "\nNo active tasks. Use `/kanban add <title>`.");
        return out.trim_end().to_string();
    }

    let mut tasks = board.tasks.clone();
    tasks.sort_by(|a, b| {
        a.lane
            .as_str()
            .cmp(b.lane.as_str())
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.id.cmp(&b.id))
    });
    let _ = writeln!(&mut out, "\nActive tasks (top 20):");
    for task in tasks.into_iter().take(20) {
        let assignee = task.assignee.unwrap_or_else(|| "-".to_string());
        let blocked = task
            .blocked_reason
            .as_deref()
            .map(|reason| format!(" blocked={reason}"))
            .unwrap_or_default();
        let bg = task
            .background_job_id
            .as_deref()
            .map(|job_id| format!(" job={job_id}"))
            .unwrap_or_default();
        let _ = writeln!(
            &mut out,
            "- {} [{}] p{} @{} {}{}{}",
            task.id,
            task.lane.as_str(),
            task.priority,
            assignee,
            task.title,
            blocked,
            bg
        );
    }
    out.trim_end().to_string()
}

fn parse_kanban_add(args: &[&str]) -> Result<NewKanbanTaskInput, AgentError> {
    if args.is_empty() {
        return Err(AgentError::Config(
            "Usage: /kanban add <title> [--lane <todo|doing|blocked|done>] [--priority <1..5>] [--assignee <name>] [--depends K-0001,K-0002] [--desc <text>] [--goal] [--goal-max-turns N]".to_string(),
        ));
    }
    let mut lane = KanbanLane::Todo;
    let mut priority: u8 = 3;
    let mut assignee: Option<String> = None;
    let mut depends_on: Vec<String> = Vec::new();
    let mut description: Option<String> = None;
    let mut goal_mode = false;
    let mut goal_max_turns: Option<u32> = None;
    let mut title_parts: Vec<String> = Vec::new();

    let mut idx = 0usize;
    while idx < args.len() {
        let token = args[idx];
        if token == "--lane" {
            idx = idx.saturating_add(1);
            let Some(raw) = args.get(idx) else {
                return Err(AgentError::Config("Missing value for --lane".to_string()));
            };
            lane = KanbanLane::parse(raw).ok_or_else(|| {
                AgentError::Config(format!(
                    "Invalid lane `{raw}`. Use: todo|doing|blocked|done."
                ))
            })?;
        } else if token == "--priority" || token == "-p" {
            idx = idx.saturating_add(1);
            let Some(raw) = args.get(idx) else {
                return Err(AgentError::Config(
                    "Missing value for --priority".to_string(),
                ));
            };
            priority = raw.parse::<u8>().map_err(|_| {
                AgentError::Config(format!("Invalid priority `{raw}`. Expected integer 1..5."))
            })?;
            if !(1..=5).contains(&priority) {
                return Err(AgentError::Config(format!(
                    "Invalid priority `{priority}`. Expected 1..5."
                )));
            }
        } else if token == "--assignee" || token == "-a" {
            idx = idx.saturating_add(1);
            assignee = args.get(idx).map(|s| s.to_string());
        } else if token == "--depends" || token == "--deps" {
            idx = idx.saturating_add(1);
            if let Some(raw) = args.get(idx) {
                depends_on = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
        } else if token == "--desc" || token == "--description" {
            idx = idx.saturating_add(1);
            description = args.get(idx).map(|s| s.to_string());
        } else if token == "--goal" || token == "--goal-mode" {
            goal_mode = true;
        } else if token == "--goal-max-turns" {
            idx = idx.saturating_add(1);
            let Some(raw) = args.get(idx) else {
                return Err(AgentError::Config(
                    "Missing value for --goal-max-turns".to_string(),
                ));
            };
            let turns = raw.parse::<u32>().map_err(|_| {
                AgentError::Config(format!(
                    "Invalid goal max turns `{raw}`. Expected positive integer."
                ))
            })?;
            if turns == 0 {
                return Err(AgentError::Config(
                    "Invalid goal max turns `0`. Expected positive integer.".to_string(),
                ));
            }
            goal_max_turns = Some(turns);
        } else {
            title_parts.push(token.to_string());
        }
        idx = idx.saturating_add(1);
    }
    let title = title_parts.join(" ").trim().to_string();
    if title.is_empty() {
        return Err(AgentError::Config(
            "Usage: /kanban add <title> [flags...]".to_string(),
        ));
    }
    Ok(NewKanbanTaskInput {
        title,
        lane,
        priority,
        assignee,
        description,
        depends_on,
        goal_mode,
        goal_max_turns,
    })
}

pub fn run_kanban_command(args: &[&str]) -> Result<String, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mut store = load_store()?;

    match action.as_str() {
        "status" | "show" => {
            let requested = args.get(1).copied();
            let board = ensure_board(&mut store, requested);
            Ok(render_kanban_status(board))
        }
        "boards" | "list" => {
            let mut out = String::from("Kanban boards:\n");
            for board in &store.boards {
                let marker = if board.id == store.current_board_id {
                    "*"
                } else {
                    " "
                };
                let project = board.project_path.as_deref().unwrap_or("-");
                let _ = writeln!(
                    &mut out,
                    "{} {} ({}) project={}",
                    marker, board.name, board.id, project
                );
            }
            Ok(out.trim_end().to_string())
        }
        "init" => {
            let Some(name) = args.get(1).copied() else {
                return Ok(
                    "Usage: kanban init <board-name> [project-path]\nExample: hermes kanban init alpha ~/Documents/Projects/hermes-agent-ultra"
                        .to_string(),
                );
            };
            let project_path = args.get(2).map(|s| s.to_string());
            let (board_name, board_id, board_snapshot) = {
                let board = create_or_select_board(&mut store, name, project_path);
                (board.name.clone(), board.id.clone(), board.clone())
            };
            save_store(&store)?;
            let checkpoint = maybe_checkpoint_to_contextlattice(
                &board_snapshot,
                KanbanActionInput {
                    action: "init".to_string(),
                    task_id: None,
                    lane: None,
                    summary: format!("board={board_name} board_id={board_id}"),
                },
            );
            Ok(format!(
                "Board selected: {} ({})\n{}",
                board_name, board_id, checkpoint.detail
            ))
        }
        "use" | "select" => {
            let Some(name_or_id) = args.get(1).copied() else {
                return Ok("Usage: kanban use <board-id-or-name>".to_string());
            };
            let (board_name, board_id) = {
                let board = ensure_board(&mut store, Some(name_or_id));
                (board.name.clone(), board.id.clone())
            };
            save_store(&store)?;
            Ok(format!("Using board: {} ({})", board_name, board_id))
        }
        "add" => {
            let input = parse_kanban_add(args.get(1..).unwrap_or_default())?;
            let (task_id, task_lane, task_priority, task_title, task_goal_mode, board_snapshot) = {
                let board = ensure_board(&mut store, None);
                let task = add_task(board, input);
                (
                    task.id.clone(),
                    task.lane,
                    task.priority,
                    task.title.clone(),
                    task.goal_mode,
                    board.clone(),
                )
            };
            save_store(&store)?;
            let checkpoint = maybe_checkpoint_to_contextlattice(
                &board_snapshot,
                KanbanActionInput {
                    action: "add".to_string(),
                    task_id: Some(task_id.clone()),
                    lane: Some(task_lane),
                    summary: task_title.clone(),
                },
            );
            Ok(format!(
                "Added task {} [{}] p{}{}: {}\n{}",
                task_id,
                task_lane.as_str(),
                task_priority,
                if task_goal_mode { " goal" } else { "" },
                task_title,
                checkpoint.detail
            ))
        }
        "attach" | "attachment" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok("Usage: kanban attach <task-id|title> <file-path>".to_string());
            };
            let Some(path) = args.get(2).copied() else {
                return Ok("Usage: kanban attach <task-id|title> <file-path>".to_string());
            };
            let (task_id, attachment, board_snapshot) = {
                let board = ensure_board(&mut store, None);
                let attachment = add_attachment_to_task(
                    board,
                    task_ref,
                    path,
                    std::env::var("HERMES_PROFILE").ok(),
                )?;
                let task_id = find_task_mut(board, task_ref)
                    .map(|task| task.id.clone())
                    .unwrap_or_else(|| task_ref.to_string());
                (task_id, attachment, board.clone())
            };
            save_store(&store)?;
            let checkpoint = maybe_checkpoint_to_contextlattice(
                &board_snapshot,
                KanbanActionInput {
                    action: "attach".to_string(),
                    task_id: Some(task_id.clone()),
                    lane: None,
                    summary: format!(
                        "attachment_id={} filename={} path={}",
                        attachment.id, attachment.filename, attachment.stored_path
                    ),
                },
            );
            Ok(format!(
                "Attached {} to {} as {} ({})\nPath: {}\n{}",
                attachment.filename,
                task_id,
                attachment.id,
                attachment.size,
                attachment.stored_path,
                checkpoint.detail
            ))
        }
        "attachments" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok("Usage: kanban attachments <task-id|title>".to_string());
            };
            let board = ensure_board(&mut store, None);
            let Some(task) = board.tasks.iter().find(|task| {
                task.id.eq_ignore_ascii_case(task_ref) || task.title.eq_ignore_ascii_case(task_ref)
            }) else {
                return Ok(format!("Task not found: {task_ref}"));
            };
            if task.attachments.is_empty() {
                return Ok(format!("No attachments for {}.", task.id));
            }
            let mut out = format!("Attachments for {}:\n", task.id);
            for attachment in &task.attachments {
                let _ = writeln!(
                    &mut out,
                    "- {} {} size={} path={}",
                    attachment.id, attachment.filename, attachment.size, attachment.stored_path
                );
            }
            Ok(out.trim_end().to_string())
        }
        "detach" | "remove-attachment" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok(
                    "Usage: kanban detach <task-id|title> <attachment-id|filename>".to_string(),
                );
            };
            let Some(attachment_ref) = args.get(2).copied() else {
                return Ok(
                    "Usage: kanban detach <task-id|title> <attachment-id|filename>".to_string(),
                );
            };
            let removed = {
                let board = ensure_board(&mut store, None);
                remove_attachment_from_task(board, task_ref, attachment_ref)?
                    .map(|attachment| (attachment, board.clone()))
            };
            if let Some((attachment, board_snapshot)) = removed {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "detach".to_string(),
                        task_id: Some(task_ref.to_string()),
                        lane: None,
                        summary: format!(
                            "attachment_id={} filename={}",
                            attachment.id, attachment.filename
                        ),
                    },
                );
                Ok(format!(
                    "Removed attachment {} ({})\n{}",
                    attachment.id, attachment.filename, checkpoint.detail
                ))
            } else {
                Ok(format!(
                    "Attachment not found: task={} attachment={}",
                    task_ref, attachment_ref
                ))
            }
        }
        "move" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok(
                    "Usage: kanban move <task-id|title> <todo|doing|blocked|done> [summary]"
                        .to_string(),
                );
            };
            let Some(raw_lane) = args.get(2).copied() else {
                return Ok(
                    "Usage: kanban move <task-id|title> <todo|doing|blocked|done> [summary]"
                        .to_string(),
                );
            };
            let Some(lane) = KanbanLane::parse(raw_lane) else {
                return Ok(format!(
                    "Invalid lane `{raw_lane}`. Use: todo|doing|blocked|done."
                ));
            };
            let summary = args.get(3..).unwrap_or_default().join(" ");
            let maybe_update = {
                let board = ensure_board(&mut store, None);
                let task_meta = if let Some(task) = find_task_mut(board, task_ref) {
                    move_task(
                        task,
                        lane,
                        (!summary.trim().is_empty()).then_some(summary.clone()),
                    );
                    Some((task.id.clone(), task.title.clone()))
                } else {
                    None
                };
                task_meta.map(|(task_id, title)| (task_id, title, board.clone()))
            };
            if let Some((task_id, title, board_snapshot)) = maybe_update {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "move".to_string(),
                        task_id: Some(task_id.clone()),
                        lane: Some(lane),
                        summary: format!("{title} {}", summary.trim()).trim().to_string(),
                    },
                );
                Ok(format!(
                    "Moved {} -> {}\n{}",
                    task_id,
                    lane.as_str(),
                    checkpoint.detail
                ))
            } else {
                Ok(format!("Task not found: {task_ref}"))
            }
        }
        "claim" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok("Usage: kanban claim <task-id|title> [assignee]".to_string());
            };
            let assignee = args.get(2).map(|s| s.to_string());
            let maybe_update = {
                let board = ensure_board(&mut store, None);
                let task_meta = if let Some(task) = find_task_mut(board, task_ref) {
                    claim_task(task, assignee.clone());
                    Some((task.id.clone(), task.lane))
                } else {
                    None
                };
                task_meta.map(|(task_id, lane)| (task_id, lane, board.clone()))
            };
            if let Some((task_id, lane, board_snapshot)) = maybe_update {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "claim".to_string(),
                        task_id: Some(task_id.clone()),
                        lane: Some(lane),
                        summary: format!(
                            "assignee={}",
                            assignee.unwrap_or_else(|| "-".to_string())
                        ),
                    },
                );
                Ok(format!(
                    "Claimed {} ({})\n{}",
                    task_id, task_ref, checkpoint.detail
                ))
            } else {
                Ok(format!("Task not found: {task_ref}"))
            }
        }
        "block" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok("Usage: kanban block <task-id|title> <reason>".to_string());
            };
            let reason = args
                .get(2..)
                .unwrap_or_default()
                .join(" ")
                .trim()
                .to_string();
            if reason.is_empty() {
                return Ok("Usage: kanban block <task-id|title> <reason>".to_string());
            }
            let maybe_update = {
                let board = ensure_board(&mut store, None);
                let task_id = if let Some(task) = find_task_mut(board, task_ref) {
                    set_blocked(task, Some(reason.clone()));
                    Some(task.id.clone())
                } else {
                    None
                };
                task_id.map(|task_id| (task_id, board.clone()))
            };
            if let Some((task_id, board_snapshot)) = maybe_update {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "block".to_string(),
                        task_id: Some(task_id.clone()),
                        lane: Some(KanbanLane::Blocked),
                        summary: reason,
                    },
                );
                Ok(format!("Blocked {}\n{}", task_id, checkpoint.detail))
            } else {
                Ok(format!("Task not found: {task_ref}"))
            }
        }
        "done" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok("Usage: kanban done <task-id|title> [summary]".to_string());
            };
            let summary = args.get(2..).unwrap_or_default().join(" ");
            let maybe_update = {
                let board = ensure_board(&mut store, None);
                let task_id = if let Some(task) = find_task_mut(board, task_ref) {
                    move_task(
                        task,
                        KanbanLane::Done,
                        (!summary.trim().is_empty()).then_some(summary.clone()),
                    );
                    Some(task.id.clone())
                } else {
                    None
                };
                task_id.map(|task_id| (task_id, board.clone()))
            };
            if let Some((task_id, board_snapshot)) = maybe_update {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "done".to_string(),
                        task_id: Some(task_id.clone()),
                        lane: Some(KanbanLane::Done),
                        summary,
                    },
                );
                Ok(format!("Marked done: {}\n{}", task_id, checkpoint.detail))
            } else {
                Ok(format!("Task not found: {task_ref}"))
            }
        }
        "archive-done" | "archive" => {
            let (archived, board_snapshot) = {
                let board = ensure_board(&mut store, None);
                let archived = archive_done(board);
                (archived, board.clone())
            };
            save_store(&store)?;
            let checkpoint = maybe_checkpoint_to_contextlattice(
                &board_snapshot,
                KanbanActionInput {
                    action: "archive_done".to_string(),
                    task_id: None,
                    lane: Some(KanbanLane::Done),
                    summary: format!("archived_count={archived}"),
                },
            );
            Ok(format!(
                "Archived {} done task(s).\n{}",
                archived, checkpoint.detail
            ))
        }
        "dispatch" => {
            let Some(task_ref) = args.get(1).copied() else {
                return Ok(
                    "Usage: kanban dispatch <task-id|title> [background-task-override]".to_string(),
                );
            };
            let override_msg = args.get(2..).unwrap_or_default().join(" ");
            let dispatch_result = {
                let board = ensure_board(&mut store, None);
                if let Some(task) = find_task_mut(board, task_ref) {
                    let task_message = if override_msg.trim().is_empty() {
                        build_worker_context(task)
                    } else {
                        override_msg.clone()
                    };
                    let job = queue_background_job(&task_message)?;
                    task.background_job_id = Some(job.id.clone());
                    move_task(task, KanbanLane::Doing, None);
                    Some((task.id.clone(), job, task_message, board.clone()))
                } else {
                    None
                }
            };
            if let Some((task_id, job, task_message, board_snapshot)) = dispatch_result {
                save_store(&store)?;
                let checkpoint = maybe_checkpoint_to_contextlattice(
                    &board_snapshot,
                    KanbanActionInput {
                        action: "dispatch".to_string(),
                        task_id: Some(task_id.clone()),
                        lane: Some(KanbanLane::Doing),
                        summary: format!("job_id={} task={}", job.id, task_message),
                    },
                );
                Ok(format!(
                    "Dispatched {} as background job {}\nStatus: {}\nLogs:   {}\n{}",
                    task_id,
                    job.id,
                    job.status_path.display(),
                    job.log_path.display(),
                    checkpoint.detail
                ))
            } else {
                Ok(format!("Task not found: {task_ref}"))
            }
        }
        "sync" => {
            let board_snapshot = {
                let board = ensure_board(&mut store, None);
                board.clone()
            };
            let checkpoint = maybe_checkpoint_to_contextlattice(
                &board_snapshot,
                KanbanActionInput {
                    action: "sync".to_string(),
                    task_id: None,
                    lane: None,
                    summary: format!(
                        "manual sync tasks={} archived={}",
                        board_snapshot.tasks.len(),
                        board_snapshot.archived.len()
                    ),
                },
            );
            Ok(checkpoint.detail)
        }
        "help" => Ok(kanban_help_text().to_string()),
        _ => Ok("Unknown kanban action. Use `hermes kanban help`.".to_string()),
    }
}

fn handle_kanban_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    emit_command_output(app, run_kanban_command(args)?);
    Ok(CommandResult::Handled)
}

fn kanban_help_text() -> &'static str {
    "Kanban commands:\n  hermes kanban status [board]\n  hermes kanban boards\n  hermes kanban init <name> [project-path]\n  hermes kanban use <name-or-id>\n  hermes kanban add <title> [--lane <todo|doing|blocked|done>] [--priority <1..5>] [--assignee <name>] [--depends K-0001,K-0002] [--desc <text>] [--goal] [--goal-max-turns N]\n  hermes kanban attach <task-id|title> <file-path>\n  hermes kanban attachments <task-id|title>\n  hermes kanban detach <task-id|title> <attachment-id|filename>\n  hermes kanban move <task-id|title> <todo|doing|blocked|done> [summary]\n  hermes kanban claim <task-id|title> [assignee]\n  hermes kanban block <task-id|title> <reason>\n  hermes kanban done <task-id|title> [summary]\n  hermes kanban archive-done\n  hermes kanban dispatch <task-id|title> [background-task-override]\n  hermes kanban sync\n\nInteractive alias: /kanban <command>"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanCapabilityMode {
    Off,
    Advisory,
    Enforce,
}

impl PlanCapabilityMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disable" | "disabled" | "0" => Some(Self::Off),
            "advisory" | "warn" | "on" | "1" => Some(Self::Advisory),
            "enforce" | "strict" => Some(Self::Enforce),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

fn plan_capability_mode() -> PlanCapabilityMode {
    std::env::var("HERMES_PLAN_CAPABILITY_ROUTER")
        .ok()
        .as_deref()
        .and_then(PlanCapabilityMode::parse)
        .unwrap_or(PlanCapabilityMode::Off)
}

fn infer_plan_requirements(task: &str) -> ModelCapabilityRequirements {
    let lower = task.to_ascii_lowercase();
    let mut req = ModelCapabilityRequirements::default();

    if [
        "repo",
        "code",
        "patch",
        "implement",
        "fix",
        "test",
        "lint",
        "build",
        "deploy",
        "file",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        req.require_tools = true;
    }
    if [
        "audit",
        "parity",
        "objective",
        "investigate",
        "diagnose",
        "analysis",
        "architecture",
        "production",
        "security",
        "trading",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        req.require_reasoning = true;
    }
    if [
        "full repo",
        "entire repo",
        "all files",
        "large codebase",
        "multi-repo",
        "end to end",
        "end-to-end",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        req.require_long_context = true;
    }
    if ["image", "screenshot", "diagram", "figma"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        req.require_vision = true;
    }

    req
}

fn plan_capability_preflight(app: &App, task: &str) -> (Option<String>, bool) {
    let mode = plan_capability_mode();
    if matches!(mode, PlanCapabilityMode::Off) {
        return (None, true);
    }

    let req = infer_plan_requirements(task);
    if req.is_empty() {
        return (None, true);
    }

    let (provider, model_id) = split_provider_model(&app.current_model);
    let client = default_client();
    let caps = resolve_model_capabilities(provider, model_id, client, Some(&app.config));
    let unmet = unmet_model_requirements(caps, req);
    if unmet.is_empty() {
        return (
            Some(format!(
                "planner capability preflight: PASS ({}) for `{}`",
                req.summary(),
                app.current_model
            )),
            true,
        );
    }

    let explain_hint = format!(
        "/model explain {} --cap tools,reasoning --min-context 128000",
        app.current_model
    );
    let message = format!(
        "planner capability preflight: {} ({}) for `{}`.\nmissing: {}\nhint: run `{}` or switch with `/model` before queuing this task.",
        if matches!(mode, PlanCapabilityMode::Enforce) {
            "BLOCKED"
        } else {
            "WARN"
        },
        req.summary(),
        app.current_model,
        unmet.join(", "),
        explain_hint
    );

    let allowed = !matches!(mode, PlanCapabilityMode::Enforce);
    (Some(message), allowed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskDepthProfile {
    Shallow,
    Balanced,
    Deep,
    Max,
}

impl TaskDepthProfile {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shallow" | "fast" => Some(Self::Shallow),
            "balanced" | "default" => Some(Self::Balanced),
            "deep" | "thorough" => Some(Self::Deep),
            "max" | "exhaustive" => Some(Self::Max),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::Balanced => "balanced",
            Self::Deep => "deep",
            Self::Max => "max",
        }
    }
}

fn set_env_var_u64(key: &str, value: u64) {
    std::env::set_var(key, value.to_string());
}

fn set_env_var_f64(key: &str, value: f64) {
    std::env::set_var(key, format!("{value:.2}"));
}

fn apply_task_depth_profile(profile: TaskDepthProfile) {
    std::env::set_var("HERMES_TASK_DEPTH_PROFILE", profile.as_str());
    match profile {
        TaskDepthProfile::Shallow => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 18);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 10);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 1);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 6);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 2800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 5200.0);
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "aggressive");
        }
        TaskDepthProfile::Balanced => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 50);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 12);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 8);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 3500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 6500.0);
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        TaskDepthProfile::Deep => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 120);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 6);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 3);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 10);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 4800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 9000.0);
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "relaxed");
        }
        TaskDepthProfile::Max => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 250);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 5);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 12);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 6500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 12000.0);
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
    }
}

fn current_task_depth_profile() -> TaskDepthProfile {
    std::env::var("HERMES_TASK_DEPTH_PROFILE")
        .ok()
        .as_deref()
        .and_then(TaskDepthProfile::parse)
        .unwrap_or(TaskDepthProfile::Balanced)
}

fn task_depth_runtime_summary() -> String {
    let profile = current_task_depth_profile();
    let max_iters = std::env::var("HERMES_MAX_ITERATIONS").unwrap_or_else(|_| "50".to_string());
    let tool_concurrency =
        std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY").unwrap_or_else(|_| "12".to_string());
    let delegate_depth =
        std::env::var("HERMES_MAX_DELEGATE_DEPTH").unwrap_or_else(|_| "4".to_string());
    let repo_budget =
        std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE").unwrap_or_else(|_| "off".to_string());
    format!(
        "task_depth profile={} max_iterations={} tool_concurrency={} max_delegate_depth={} repo_budget_profile={}",
        profile.as_str(),
        max_iters,
        tool_concurrency,
        delegate_depth,
        repo_budget
    )
}

fn handle_plan_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "help" | "usage"))
    {
        emit_command_output(
            app,
            "Planner controls:\n  /plan <task>          Queue a planning/research task in background\n  /plan status          Show queue health + active steering\n  /plan list            Show queue health + active steering\n  /plan clear           Clear queued/running status records\n  /plan caps [mode]     Optional capability router (`off|advisory|enforce`)\n  /plan depth [profile] Task-depth governor (`status|list|shallow|balanced|deep|max|clear`)",
        );
        return Ok(CommandResult::Handled);
    }

    let sub = args[0].to_ascii_lowercase();
    if sub == "caps" || sub == "capability" || sub == "capabilities" {
        let next = args
            .get(1)
            .copied()
            .unwrap_or("status")
            .to_ascii_lowercase();
        match next.as_str() {
            "status" | "show" => {
                emit_command_output(
                    app,
                    format!(
                        "planner capability router mode={}\nUse `/plan caps [off|advisory|enforce]`.",
                        plan_capability_mode().as_str()
                    ),
                );
            }
            "off" | "advisory" | "enforce" => {
                if let Some(mode) = PlanCapabilityMode::parse(&next) {
                    std::env::set_var("HERMES_PLAN_CAPABILITY_ROUTER", mode.as_str());
                    emit_command_output(
                        app,
                        format!("planner capability router set to `{}`.", mode.as_str()),
                    );
                }
            }
            _ => emit_command_output(app, "Usage: /plan caps [status|off|advisory|enforce]"),
        }
        return Ok(CommandResult::Handled);
    }
    if sub == "depth" {
        let next = args
            .get(1)
            .copied()
            .unwrap_or("status")
            .to_ascii_lowercase();
        match next.as_str() {
            "status" | "show" => emit_command_output(app, task_depth_runtime_summary()),
            "list" => emit_command_output(
                app,
                "Task depth profiles:\n- shallow: quickest turn cadence; strict exploration trim\n- balanced: default profile for most sessions\n- deep: larger turn budget + lower concurrency for heavier analysis\n- max: exhaustive mode for very complex objective work\nUse `/plan depth <profile>` to apply.",
            ),
            "clear" => {
                std::env::remove_var("HERMES_TASK_DEPTH_PROFILE");
                for key in [
                    "HERMES_MAX_ITERATIONS",
                    "HERMES_TOOL_CALL_MAX_CONCURRENCY",
                    "HERMES_MAX_DELEGATE_DEPTH",
                    "HERMES_PERF_GOV_WINDOW",
                    "HERMES_PERF_GOV_LATENCY_WARN_MS",
                    "HERMES_PERF_GOV_LATENCY_CRITICAL_MS",
                    "HERMES_REPO_REVIEW_BUDGET_PROFILE",
                ] {
                    std::env::remove_var(key);
                }
                apply_task_depth_profile(TaskDepthProfile::Balanced);
                emit_command_output(
                    app,
                    format!("Task depth reset to defaults.\n{}", task_depth_runtime_summary()),
                );
            }
            _ => {
                let Some(profile) = TaskDepthProfile::parse(&next) else {
                    emit_command_output(
                        app,
                        "Usage: /plan depth [status|list|shallow|balanced|deep|max|clear]",
                    );
                    return Ok(CommandResult::Handled);
                };
                apply_task_depth_profile(profile);
                emit_command_output(
                    app,
                    format!(
                        "Task depth profile set to `{}`.\n{}",
                        profile.as_str(),
                        task_depth_runtime_summary()
                    ),
                );
            }
        }
        return Ok(CommandResult::Handled);
    }
    if sub == "status" || sub == "list" {
        let (queued, running, completed, failed) = background_job_counts();
        let mut out = String::new();
        let _ = writeln!(out, "Planner queue status");
        let _ = writeln!(
            out,
            "  queued={} running={} completed={} failed={}",
            queued, running, completed, failed
        );
        if let Some(steer) = current_session_steer(app) {
            let _ = writeln!(out, "  steering={}", truncate_chars(&steer, 160));
        } else {
            let _ = writeln!(out, "  steering=(none)");
        }
        if let Some(objective) = app.session_objective.as_deref() {
            let _ = writeln!(out, "  objective={}", truncate_chars(objective, 160));
        } else {
            let _ = writeln!(out, "  objective=(none)");
        }
        let _ = writeln!(
            out,
            "  capability_router={}",
            plan_capability_mode().as_str()
        );
        let _ = writeln!(out, "  {}", task_depth_runtime_summary());
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }
    if sub == "clear" {
        return handle_clear_queue_command(app);
    }
    let task = args.join(" ");
    if !task.trim().is_empty() {
        let (note, allowed) = plan_capability_preflight(app, &task);
        if let Some(msg) = note {
            emit_command_output(app, msg);
        }
        if !allowed {
            return Ok(CommandResult::Handled);
        }
    }
    handle_background_command(app, args)
}

fn handle_lsp_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match sub.as_str() {
        "status" | "show" => {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unavailable>".to_string());
            let mut out = String::new();
            let _ = writeln!(out, "LSP/code-index status");
            let _ = writeln!(out, "  cwd: {}", cwd);
            let _ = writeln!(
                out,
                "  code_index_enabled: {}",
                yes_no(app.config.agent.code_index_enabled)
            );
            let _ = writeln!(
                out,
                "  code_index_max_files: {}",
                app.config.agent.code_index_max_files
            );
            let _ = writeln!(
                out,
                "  code_index_max_symbols: {}",
                app.config.agent.code_index_max_symbols
            );
            let _ = writeln!(
                out,
                "  lsp_context_enabled: {}",
                yes_no(app.config.agent.lsp_context_enabled)
            );
            let _ = writeln!(
                out,
                "  lsp_context_max_chars: {}",
                app.config.agent.lsp_context_max_chars
            );
            let _ = writeln!(
                out,
                "  tip: run `/plan map the repo architecture` to force a high-signal repo-map pass."
            );
            emit_command_output(app, out.trim_end());
        }
        "refresh" => {
            emit_command_output(
                app,
                "Code index refresh is automatic while the agent executes tool calls. Queue a focused analysis with `/plan <task>` if you want a deliberate repo-map rebuild now.",
            );
        }
        "help" => {
            emit_command_output(
                app,
                "Usage: /lsp [status|refresh]\n  status   show code-index + LSP context configuration\n  refresh  explain how to trigger a fresh index pass",
            );
        }
        _ => emit_command_output(app, "Usage: /lsp [status|refresh]"),
    }
    Ok(CommandResult::Handled)
}

fn collect_graph_candidate_files(
    root: &Path,
    max_files: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), AgentError> {
    if out.len() >= max_files {
        return Ok(());
    }
    let rd = std::fs::read_dir(root)
        .map_err(|e| AgentError::Io(format!("read_dir {}: {}", root.display(), e)))?;
    for entry in rd {
        if out.len() >= max_files {
            break;
        }
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if matches!(
                name,
                ".git"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | ".mypy_cache"
                    | ".pytest_cache"
            ) {
                continue;
            }
            collect_graph_candidate_files(&path, max_files, out)?;
            continue;
        }
        let ext = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "rs" | "py" | "ts" | "tsx" | "js" | "jsx") {
            out.push(path);
        }
    }
    Ok(())
}

fn extract_semantic_refs_for_file(ext: &str, content: &str) -> Vec<String> {
    let mut refs = Vec::new();
    match ext {
        "rs" => {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    let target = rest.split(';').next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
                if let Some(rest) = trimmed.strip_prefix("mod ") {
                    let target = rest.split(';').next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    for item in rest.split(',') {
                        let target = item.split_whitespace().next().unwrap_or_default().trim();
                        if !target.is_empty() {
                            refs.push(target.to_string());
                        }
                    }
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    let target = rest.split_whitespace().next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
            }
        }
        "ts" | "tsx" | "js" | "jsx" => {
            let re = Regex::new(r#"(?m)from\s+["']([^"']+)["']"#).expect("valid import regex");
            for caps in re.captures_iter(content) {
                if let Some(m) = caps.get(1) {
                    refs.push(m.as_str().trim().to_string());
                }
            }
            let re_req = Regex::new(r#"(?m)require\(\s*["']([^"']+)["']\s*\)"#)
                .expect("valid require regex");
            for caps in re_req.captures_iter(content) {
                if let Some(m) = caps.get(1) {
                    refs.push(m.as_str().trim().to_string());
                }
            }
        }
        _ => {}
    }
    refs
}

fn sanitize_graph_node(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn contextlattice_base_url_for_graph() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
}

fn contextlattice_api_key_for_graph() -> Option<String> {
    std::env::var("CONTEXTLATTICE_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("MEMMCP_API_KEY").ok())
        .filter(|v| !v.trim().is_empty())
}

fn extract_json_path<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur)
}

fn extract_embedding_diag_line(payload: &serde_json::Value) -> String {
    let backend = [
        &["backend"][..],
        &["embedding_backend"][..],
        &["embeddings", "backend"][..],
        &["retrieval", "embedding_backend"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_str())
    .unwrap_or("unknown");
    let dimension = [
        &["dimension"][..],
        &["embeddings", "dimension"][..],
        &["retrieval", "embedding_dimension"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_u64())
    .map(|v| v.to_string())
    .unwrap_or_else(|| "n/a".to_string());
    let model = [
        &["model"][..],
        &["embeddings", "model"][..],
        &["retrieval", "embedding_model"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_str())
    .unwrap_or("unknown");
    format!(
        "embedding_diagnostics: backend={} model={} dimension={}",
        backend, model, dimension
    )
}

async fn contextlattice_embedding_diagnostics_lines() -> Vec<String> {
    let base_url = contextlattice_base_url_for_graph();
    let mut lines = Vec::new();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            lines.push(format!("client_error: {}", err));
            return lines;
        }
    };

    let mut health_req = client.get(format!("{}/health", base_url.trim_end_matches('/')));
    if let Some(key) = contextlattice_api_key_for_graph() {
        health_req = health_req.header("x-api-key", key);
    }
    match health_req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            lines.push(format!("health_status: {}", code));
        }
        Err(err) => {
            lines.push(format!("health_status: unreachable ({})", err));
        }
    }

    let mut emb_req = client.get(format!(
        "{}/telemetry/embeddings",
        base_url.trim_end_matches('/')
    ));
    if let Some(key) = contextlattice_api_key_for_graph() {
        emb_req = emb_req.header("x-api-key", key);
    }
    match emb_req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(payload) => lines.push(extract_embedding_diag_line(&payload)),
                    Err(err) => {
                        lines.push(format!("embedding_diagnostics: invalid_json ({})", err))
                    }
                }
            } else {
                lines.push(format!(
                    "embedding_diagnostics: unavailable (telemetry/embeddings status={})",
                    status.as_u16()
                ));
                lines.push("embedding_diagnostics: fallback=recall_telemetry".to_string());
            }
        }
        Err(err) => {
            lines.push(format!(
                "embedding_diagnostics: unavailable (unreachable: {})",
                err
            ));
            lines.push("embedding_diagnostics: fallback=recall_telemetry".to_string());
        }
    }

    let mut recall_req = client.get(format!(
        "{}/telemetry/recall",
        base_url.trim_end_matches('/')
    ));
    if let Some(key) = contextlattice_api_key_for_graph() {
        recall_req = recall_req.header("x-api-key", key);
    }
    match recall_req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(payload) => {
                let qps = payload
                    .get("query_per_sec")
                    .or_else(|| payload.get("qps"))
                    .and_then(|v| v.as_f64())
                    .map(|v| format!("{:.3}", v))
                    .unwrap_or_else(|| "n/a".to_string());
                let hit_rate = payload
                    .get("hit_rate")
                    .or_else(|| payload.get("grounded_hit_rate"))
                    .and_then(|v| v.as_f64())
                    .map(|v| format!("{:.3}", v))
                    .unwrap_or_else(|| "n/a".to_string());
                lines.push(format!(
                    "recall_telemetry: qps={} hit_rate={}",
                    qps, hit_rate
                ));
            }
            Err(err) => lines.push(format!("recall_telemetry: invalid_json ({})", err)),
        },
        Ok(resp) => lines.push(format!(
            "recall_telemetry: endpoint_status={}",
            resp.status()
        )),
        Err(err) => lines.push(format!("recall_telemetry: unreachable ({})", err)),
    }

    lines
}

include!("command_kanban_objective/graph_objective.rs");
