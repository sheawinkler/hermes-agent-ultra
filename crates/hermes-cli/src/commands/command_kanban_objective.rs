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
    let caps = resolve_model_capabilities(provider, model_id, client);
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

async fn handle_graph_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match sub.as_str() {
        "status" | "show" => {
            let contextlattice_mcp = app.config.mcp_servers.iter().any(|entry| {
                let name = entry.name.to_ascii_lowercase();
                let url = entry
                    .url
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                name.contains("contextlattice") || url.contains("contextlattice")
            });
            let policy = load_contextlattice_policy().ok();
            let mut out = String::new();
            let _ = writeln!(out, "Graph-memory status");
            let _ = writeln!(out, "  contextlattice_mcp: {}", yes_no(contextlattice_mcp));
            let diag = contextlattice_embedding_diagnostics_lines().await;
            for row in &diag {
                let _ = writeln!(out, "  {}", row);
            }
            if let Some(policy) = policy {
                let _ = writeln!(
                    out,
                    "  retrieval_mode_hint: {}",
                    policy.preferred_retrieval_mode
                );
                let _ = writeln!(out, "  preflight_required: {}", policy.preflight_required);
                let _ = writeln!(
                    out,
                    "  include_grounding_required: {}",
                    policy.include_grounding_required
                );
                let _ = writeln!(
                    out,
                    "  degradation_aware_planning: {}",
                    policy.degradation_aware_planning
                );
            } else {
                let _ = writeln!(out, "  contextlattice_policy: unavailable");
            }
            emit_command_output(app, out.trim_end());
        }
        "embeddings" | "embedding" | "diag" => {
            let mut out = String::new();
            let _ = writeln!(out, "ContextLattice embedding diagnostics");
            let _ = writeln!(out, "base_url: {}", contextlattice_base_url_for_graph());
            let lines = contextlattice_embedding_diagnostics_lines().await;
            if lines.is_empty() {
                out.push_str("no diagnostic lines returned.");
            } else {
                for line in lines {
                    let _ = writeln!(out, "- {}", line);
                }
            }
            out.push_str("\nIf endpoint support is partial, Hermes falls back to `/telemetry/recall` snapshots.");
            emit_command_output(app, out.trim_end());
        }
        "repo" | "semantic" => {
            let mut max_files = 220usize;
            let mut repo_arg: Option<&str> = None;
            let mut idx = 1usize;
            while idx < args.len() {
                if args[idx] == "--max-files" {
                    if let Some(raw) = args.get(idx + 1).copied() {
                        if let Ok(parsed) = raw.parse::<usize>() {
                            max_files = parsed.clamp(20, 1500);
                        }
                        idx += 2;
                        continue;
                    }
                }
                repo_arg = Some(args[idx]);
                idx += 1;
            }
            let repo_root = if let Some(raw) = repo_arg {
                PathBuf::from(raw)
            } else {
                std::env::current_dir()
                    .map_err(|e| AgentError::Io(format!("current_dir: {}", e)))?
            };
            if !repo_root.exists() {
                emit_command_output(
                    app,
                    format!("Repo path does not exist: {}", repo_root.display()),
                );
                return Ok(CommandResult::Handled);
            }

            let mut files = Vec::new();
            collect_graph_candidate_files(&repo_root, max_files, &mut files)?;
            if files.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No candidate source files found under {} (max_files={}).",
                        repo_root.display(),
                        max_files
                    ),
                );
                return Ok(CommandResult::Handled);
            }

            let mut edges: HashMap<(String, String), usize> = HashMap::new();
            let mut node_degree: HashMap<String, usize> = HashMap::new();
            for path in &files {
                let rel = path
                    .strip_prefix(&repo_root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let ext = path
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let content = std::fs::read_to_string(path).unwrap_or_default();
                for rf in extract_semantic_refs_for_file(&ext, &content) {
                    let key = (rel.clone(), rf.clone());
                    *edges.entry(key).or_insert(0usize) += 1;
                    *node_degree.entry(rel.clone()).or_insert(0usize) += 1;
                    *node_degree.entry(rf).or_insert(0usize) += 1;
                }
            }

            let mut degree_ranked: Vec<(String, usize)> = node_degree.into_iter().collect();
            degree_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut edge_ranked: Vec<((String, String), usize)> = edges.into_iter().collect();
            edge_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            let mut out = String::new();
            let _ = writeln!(out, "Semantic repo graph");
            let _ = writeln!(out, "  repo_root={}", repo_root.display());
            let _ = writeln!(out, "  files_scanned={} (cap={})", files.len(), max_files);
            let _ = writeln!(out, "  semantic_edges={}", edge_ranked.len());
            let _ = writeln!(out);
            let _ = writeln!(out, "Top hubs (degree):");
            for (idx, (node, degree)) in degree_ranked.iter().take(12).enumerate() {
                let _ = writeln!(out, "  {}. {} ({})", idx + 1, node, degree);
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "Top semantic edges:");
            for (idx, ((src, dst), weight)) in edge_ranked.iter().take(16).enumerate() {
                let _ = writeln!(out, "  {}. {} -> {} ({})", idx + 1, src, dst, weight);
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "Mermaid preview:");
            let _ = writeln!(out, "```mermaid");
            let _ = writeln!(out, "graph LR");
            for ((src, dst), _) in edge_ranked.iter().take(32) {
                let src_n = sanitize_graph_node(src);
                let dst_n = sanitize_graph_node(dst);
                let _ = writeln!(out, "  {}[\"{}\"] --> {}[\"{}\"]", src_n, src, dst_n, dst);
            }
            let _ = writeln!(out, "```");
            emit_command_output(app, out.trim_end());
        }
        "help" => emit_command_output(
            app,
            "Usage: /graph [status|embeddings|repo [path] [--max-files N]]",
        ),
        _ => emit_command_output(
            app,
            "Usage: /graph [status|embeddings|repo [path] [--max-files N]]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_image_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let status = app
            .pending_image_hint()
            .map(|path| {
                format!(
                    "Pending image hint: {}\nUse `/image clear` to remove it.",
                    path
                )
            })
            .unwrap_or_else(|| {
                "No pending image hint.\nUsage: /image <path> | /image clear".to_string()
            });
        emit_command_output(app, status);
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("clear") {
        app.clear_pending_image_hint();
        emit_command_output(app, "Cleared pending image hint.");
        return Ok(CommandResult::Handled);
    }

    let path = args.join(" ").trim().to_string();
    if path.is_empty() {
        emit_command_output(app, "Usage: /image <path> | /image clear");
        return Ok(CommandResult::Handled);
    }
    let exists = Path::new(&path).exists();
    app.set_pending_image_hint(path.clone());
    if exists {
        emit_command_output(
            app,
            format!(
                "Image hint queued: `{}`.\nIt will be injected into the next prompt automatically.",
                path
            ),
        );
    } else {
        emit_command_output(
            app,
            format!(
                "Image hint queued: `{}` (path not found right now).\nIt will still be injected into the next prompt.",
                path
            ),
        );
    }
    Ok(CommandResult::Handled)
}

fn apply_objective_lifecycle_update(
    app: &mut App,
    raw_status: &str,
    reason: Option<&str>,
) -> Result<CommandResult, AgentError> {
    let reason_owned = reason
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let updated = set_objective_contract_lifecycle_status(raw_status, reason_owned.as_deref())?;
    let status = canonical_objective_lifecycle_status(&updated.lifecycle_status);
    let objective_injected = objective_lifecycle_is_active(&status);
    if objective_injected {
        app.set_session_objective(Some(updated.objective_text.clone()));
    } else {
        app.set_session_objective(None);
    }
    let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
        recorded_at: String::new(),
        objective_id: updated.id.clone(),
        objective_state: status.clone(),
        decision: format!("objective_status_{}", status),
        evidence_files: vec!["alpha/objective_contract.json".to_string()],
        evidence_commands: vec![format!("/objective lifecycle {}", status)],
        notes: format!(
            "Objective lifecycle set to {}. reason={}",
            status, updated.status_reason
        ),
    });
    let mut out = String::new();
    out.push_str("Objective lifecycle updated\n");
    out.push_str("-------------------------\n");
    let _ = writeln!(out, "objective_id={}", updated.id);
    let _ = writeln!(out, "status={}", status);
    let _ = writeln!(out, "reason={}", updated.status_reason);
    let _ = writeln!(out, "objective_injected={}", yes_no(objective_injected));
    let _ = writeln!(
        out,
        "behavior_mode={}",
        canonical_objective_behavior_mode(&updated.behavior_mode)
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

enum ObjectiveWaitRequest {
    Pid(u32, String),
    Session(String, String),
    Seconds(u64, String),
}

fn parse_objective_wait_seconds(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_suffix = trimmed
        .strip_suffix('s')
        .or_else(|| trimmed.strip_suffix("sec"))
        .or_else(|| trimmed.strip_suffix("secs"))
        .or_else(|| trimmed.strip_suffix("seconds"))
        .unwrap_or(trimmed);
    without_suffix.trim().parse::<u64>().ok().filter(|v| *v > 0)
}

fn parse_objective_wait_request(args: &[&str]) -> Result<ObjectiveWaitRequest, String> {
    let Some(raw_target) = args.get(1).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
        return Err(
            "Usage: /objective wait <pid|session_id|session:<id>|--session <id>|--seconds <n>|for <n>s> [reason...]"
                .to_string(),
        );
    };
    let lower = raw_target.to_ascii_lowercase();
    let reason_from = |start: usize| -> String {
        args.get(start..)
            .unwrap_or(&[])
            .join(" ")
            .trim()
            .to_string()
    };

    if matches!(lower.as_str(), "--session" | "session" | "sid") {
        let Some(session_id) = args.get(2).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
            return Err("Usage: /objective wait --session <session_id> [reason...]".to_string());
        };
        return Ok(ObjectiveWaitRequest::Session(
            session_id.to_string(),
            reason_from(3),
        ));
    }

    if matches!(
        lower.as_str(),
        "--seconds" | "seconds" | "second" | "secs" | "sec" | "for" | "timer"
    ) {
        let Some(raw_seconds) = args.get(2).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
            return Err("Usage: /objective wait --seconds <seconds> [reason...]".to_string());
        };
        let seconds = parse_objective_wait_seconds(raw_seconds)
            .ok_or_else(|| "objective wait seconds must be positive".to_string())?;
        return Ok(ObjectiveWaitRequest::Seconds(seconds, reason_from(3)));
    }

    for prefix in ["session:", "sid:"] {
        if let Some(session_id) = raw_target.strip_prefix(prefix) {
            let session_id = session_id.trim();
            if session_id.is_empty() {
                return Err("objective wait session id cannot be empty".to_string());
            }
            return Ok(ObjectiveWaitRequest::Session(
                session_id.to_string(),
                reason_from(2),
            ));
        }
    }

    for prefix in ["seconds:", "secs:", "sec:", "for:"] {
        if let Some(raw_seconds) = raw_target.strip_prefix(prefix) {
            let seconds = parse_objective_wait_seconds(raw_seconds)
                .ok_or_else(|| "objective wait seconds must be positive".to_string())?;
            return Ok(ObjectiveWaitRequest::Seconds(seconds, reason_from(2)));
        }
    }

    if let Some(seconds) = parse_objective_wait_seconds(raw_target).filter(|_| {
        raw_target.ends_with('s') || lower.ends_with("sec") || lower.ends_with("seconds")
    }) {
        return Ok(ObjectiveWaitRequest::Seconds(seconds, reason_from(2)));
    }

    if let Ok(pid) = raw_target.parse::<u32>() {
        if pid == 0 {
            return Err("objective wait pid must be positive".to_string());
        }
        return Ok(ObjectiveWaitRequest::Pid(pid, reason_from(2)));
    }

    Ok(ObjectiveWaitRequest::Session(
        raw_target.to_string(),
        reason_from(2),
    ))
}

fn apply_objective_wait_request(
    app: &mut App,
    request: ObjectiveWaitRequest,
) -> Result<CommandResult, AgentError> {
    let (updated, decision, command) = match request {
        ObjectiveWaitRequest::Pid(pid, reason) => (
            set_objective_contract_wait_pid(pid, Some(&reason))?,
            "objective_wait_pid".to_string(),
            format!("/objective wait {pid}"),
        ),
        ObjectiveWaitRequest::Session(session_id, reason) => (
            set_objective_contract_wait_session(&session_id, Some(&reason))?,
            "objective_wait_session".to_string(),
            format!("/objective wait --session {session_id}"),
        ),
        ObjectiveWaitRequest::Seconds(seconds, reason) => (
            set_objective_contract_wait_seconds(seconds, Some(&reason))?,
            "objective_wait_seconds".to_string(),
            format!("/objective wait --seconds {seconds}"),
        ),
    };

    let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
        recorded_at: String::new(),
        objective_id: updated.id.clone(),
        objective_state: canonical_objective_lifecycle_status(&updated.lifecycle_status),
        decision,
        evidence_files: vec!["alpha/objective_contract.json".to_string()],
        evidence_commands: vec![command],
        notes: format!(
            "Objective wait barrier set: {}",
            summarize_objective_wait_barrier(&updated)
        ),
    });

    let mut out = String::new();
    out.push_str("Objective wait barrier set\n");
    out.push_str("--------------------------\n");
    let _ = writeln!(out, "objective_id={}", updated.id);
    let _ = writeln!(
        out,
        "wait_barrier={}",
        summarize_objective_wait_barrier(&updated)
    );
    let _ = writeln!(out, "continuation_enforcer=parked_until_released");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_objective_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let objective_usage = "Usage: `/objective <text>` or `/objective status|verify|plan|constraints|counterfactual <scenario> | <expected_delta>|wait <pid|session_id|--session <id>|--seconds <n>|for <n>s> [reason...]|unwait|lifecycle [status|active|pause|resume|budget-limited|achieved|unmet]|behavior [status|list|balanced|strict|autonomous|mission|minimal]|profile [status|list|general|me|set <id>]|context [status|list|max|balanced|fast]|simulator [status|balanced|strict|aggressive]|ensemble [status|committee|single|debate]|ledger [status|tail [n]|clear]|dag [status|rebuild|clear]|eval [status|tail [n]]|clear`.";

    if let Some(first) = args.first() {
        let cmd = first.trim().to_ascii_lowercase();

        if cmd == "wait" {
            match parse_objective_wait_request(args) {
                Ok(request) => return apply_objective_wait_request(app, request),
                Err(usage) => {
                    emit_command_output(app, usage);
                    return Ok(CommandResult::Handled);
                }
            }
        }

        if cmd == "unwait" || cmd == "clear-wait" || cmd == "clear_wait" {
            let updated = clear_objective_contract_wait_barrier()?;
            let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
                recorded_at: String::new(),
                objective_id: updated.id.clone(),
                objective_state: canonical_objective_lifecycle_status(&updated.lifecycle_status),
                decision: "objective_unwait".to_string(),
                evidence_files: vec!["alpha/objective_contract.json".to_string()],
                evidence_commands: vec!["/objective unwait".to_string()],
                notes: "Objective wait barrier cleared by operator command.".to_string(),
            });
            emit_command_output(
                app,
                format!(
                    "Objective wait barrier cleared.\nobjective_id={}\nwait_barrier={}",
                    updated.id,
                    summarize_objective_wait_barrier(&updated)
                ),
            );
            return Ok(CommandResult::Handled);
        }

        let lifecycle_alias = match cmd.as_str() {
            "pause" => Some("paused"),
            "resume" => Some("active"),
            "active" | "pursuing" => Some("active"),
            "budget" | "budget-limited" | "budget_limited" | "limited" => Some("budget_limited"),
            "achieved" | "complete" | "done" => Some("complete"),
            "unmet" | "failed" => Some("unmet"),
            _ => None,
        };
        if let Some(status) = lifecycle_alias {
            let reason = if args.len() > 1 {
                Some(args[1..].join(" "))
            } else {
                None
            };
            return apply_objective_lifecycle_update(app, status, reason.as_deref());
        }

        if cmd == "lifecycle" || cmd == "state" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "status" || sub == "show" {
                let Some(contract) = load_objective_contract()? else {
                    emit_command_output(
                        app,
                        "No objective contract. Set one with `/objective <text>`.",
                    );
                    return Ok(CommandResult::Handled);
                };
                let status = canonical_objective_lifecycle_status(&contract.lifecycle_status);
                let objective_injected = objective_lifecycle_is_active(&status);
                let mut out = String::new();
                out.push_str("Objective lifecycle\n");
                out.push_str("-------------------\n");
                let _ = writeln!(out, "objective_id={}", contract.id);
                let _ = writeln!(out, "status={}", status);
                let _ = writeln!(out, "reason={}", contract.status_reason);
                let _ = writeln!(out, "objective_injected={}", yes_no(objective_injected));
                let _ = writeln!(
                    out,
                    "behavior_mode={}",
                    canonical_objective_behavior_mode(&contract.behavior_mode)
                );
                emit_command_output(app, out.trim_end());
                return Ok(CommandResult::Handled);
            }
            if sub == "list" {
                emit_command_output(
                    app,
                    "Lifecycle states:\n- active (alias: pursuing, resume)\n- paused (alias: pause)\n- budget_limited (alias: budget, limited)\n- complete (alias: achieved, done)\n- unmet (hard-blocked objective)",
                );
                return Ok(CommandResult::Handled);
            }
            if matches!(
                sub.as_str(),
                "active"
                    | "pursuing"
                    | "pause"
                    | "paused"
                    | "resume"
                    | "budget"
                    | "budget-limited"
                    | "budget_limited"
                    | "limited"
                    | "complete"
                    | "achieved"
                    | "done"
                    | "unmet"
                    | "failed"
            ) {
                let reason = if args.len() > 2 {
                    Some(args[2..].join(" "))
                } else {
                    None
                };
                return apply_objective_lifecycle_update(app, &sub, reason.as_deref());
            }
            emit_command_output(
                app,
                "Usage: /objective lifecycle [status|list|active|pause|resume|budget-limited|achieved|unmet] [reason...]",
            );
            return Ok(CommandResult::Handled);
        }

        if cmd == "behavior" || cmd == "mode" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "status" || sub == "show" {
                let Some(contract) = load_objective_contract()? else {
                    emit_command_output(
                        app,
                        "No objective contract. Set one with `/objective <text>`.",
                    );
                    return Ok(CommandResult::Handled);
                };
                let mut out = String::new();
                out.push_str("Objective behavior mode\n");
                out.push_str("-----------------------\n");
                let _ = writeln!(out, "objective_id={}", contract.id);
                let _ = writeln!(
                    out,
                    "mode={}",
                    canonical_objective_behavior_mode(&contract.behavior_mode)
                );
                if !contract.behavior_directives.is_empty() {
                    out.push_str("directives:\n");
                    for directive in &contract.behavior_directives {
                        let _ = writeln!(out, "- {}", directive);
                    }
                }
                if !contract.success_criteria.is_empty() {
                    out.push_str("success_criteria:\n");
                    for criterion in &contract.success_criteria {
                        let _ = writeln!(out, "- {}", criterion);
                    }
                }
                emit_command_output(app, out.trim_end());
                return Ok(CommandResult::Handled);
            }
            if sub == "list" {
                emit_command_output(
                    app,
                    "Behavior modes:\n- balanced: generalized execution with evidence checkpoints\n- strict: strongest evidence-first + contradiction discipline\n- autonomous: proactive loop execution until blocked\n- mission (aliases: sigma, perpetual): closed-loop perpetual objective improvement with concrete execution each cycle\n- minimal: concise operator-facing output with decisive actions",
                );
                return Ok(CommandResult::Handled);
            }
            let canonical_mode = canonical_objective_behavior_mode(&sub);
            if !matches!(
                canonical_mode.as_str(),
                "balanced" | "strict" | "autonomous" | "mission" | "minimal"
            ) {
                emit_command_output(
                    app,
                    "Usage: /objective behavior [status|list|balanced|strict|autonomous|mission|minimal|sigma]",
                );
                return Ok(CommandResult::Handled);
            }
            let updated = set_objective_contract_behavior_mode(&sub)?;
            let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
                recorded_at: String::new(),
                objective_id: updated.id.clone(),
                objective_state: canonical_objective_lifecycle_status(&updated.lifecycle_status),
                decision: format!(
                    "objective_behavior_{}",
                    canonical_objective_behavior_mode(&updated.behavior_mode)
                ),
                evidence_files: vec!["alpha/objective_contract.json".to_string()],
                evidence_commands: vec![format!("/objective behavior {}", sub)],
                notes: "Objective behavior mode updated by operator command.".to_string(),
            });
            let mut out = String::new();
            out.push_str("Objective behavior updated\n");
            out.push_str("-------------------------\n");
            let _ = writeln!(out, "objective_id={}", updated.id);
            let _ = writeln!(
                out,
                "mode={}",
                canonical_objective_behavior_mode(&updated.behavior_mode)
            );
            out.push_str("directives:\n");
            for directive in &updated.behavior_directives {
                let _ = writeln!(out, "- {}", directive);
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "context" || cmd == "contextlattice" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            match sub.as_str() {
                "status" | "show" => {
                    let p = load_contextlattice_policy()?;
                    let mut out = String::new();
                    out.push_str("ContextLattice policy\n");
                    out.push_str("--------------------\n");
                    let _ = writeln!(out, "mode_hint: {}", p.preferred_retrieval_mode);
                    let _ = writeln!(out, "preflight_required: {}", p.preflight_required);
                    let _ = writeln!(
                        out,
                        "auto_context_pack_on_mission_start: {}",
                        p.auto_context_pack_on_mission_start
                    );
                    let _ = writeln!(
                        out,
                        "degradation_aware_planning: {}",
                        p.degradation_aware_planning
                    );
                    let _ = writeln!(
                        out,
                        "include_grounding_required: {}",
                        p.include_grounding_required
                    );
                    let _ = writeln!(
                        out,
                        "include_retrieval_debug_for_execution: {}",
                        p.include_retrieval_debug_for_execution
                    );
                    let _ = writeln!(
                        out,
                        "broaden_scope_on_zero_hits: {}",
                        p.broaden_scope_on_zero_hits
                    );
                    let _ = writeln!(
                        out,
                        "scoped_recency_pass_before_finalize: {}",
                        p.scoped_recency_pass_before_finalize
                    );
                    let _ = writeln!(
                        out,
                        "objective_analytics_writeback_required: {}",
                        p.objective_analytics_writeback_required
                    );
                    let _ = writeln!(
                        out,
                        "contradiction_check_across_layers: {}",
                        p.contradiction_check_across_layers
                    );
                    let _ = writeln!(
                        out,
                        "numeric_fact_verbatim_copy: {}",
                        p.numeric_fact_verbatim_copy
                    );
                    let _ = writeln!(
                        out,
                        "required_project_scoping: {}",
                        p.required_project_scoping
                    );
                    let _ = writeln!(
                        out,
                        "checkpoint_payload_requires_project_file_topic: {}",
                        p.checkpoint_payload_requires_project_file_topic
                    );
                    let _ = writeln!(
                        out,
                        "readback_verification_required: {}",
                        p.readback_verification_required
                    );
                    let _ = writeln!(
                        out,
                        "conflict_resolution_mode: {}",
                        p.conflict_resolution_mode
                    );
                    let _ = writeln!(
                        out,
                        "deep_retry_budget_secs: {:?}",
                        p.deep_retry_budget_secs
                    );
                    let _ = writeln!(
                        out,
                        "regular_retry_budget_secs: {:?}",
                        p.regular_retry_budget_secs
                    );
                    let _ = writeln!(
                        out,
                        "summary_sink_order: {}",
                        p.summary_sink_order.join(",")
                    );
                    emit_command_output(app, out.trim_end());
                    return Ok(CommandResult::Handled);
                }
                "list" => {
                    emit_command_output(
                        app,
                        "ContextLattice policy presets:\n- max: full evidence + deep retrieval + strict recency/readback gates\n- balanced: full evidence with moderate deep/regular retry budgets\n- fast: grounded but lower retrieval-debug overhead for speed-sensitive loops",
                    );
                    return Ok(CommandResult::Handled);
                }
                "max" | "strict" | "balanced" | "fast" | "speed" => {
                    let p = set_contextlattice_policy_mode(&sub)?;
                    emit_command_output(
                        app,
                        format!(
                            "ContextLattice policy updated.\nmode={} preflight={} retrieval_mode={} deep_retries={:?} regular_retries={:?}",
                            sub,
                            p.preflight_required,
                            p.preferred_retrieval_mode,
                            p.deep_retry_budget_secs,
                            p.regular_retry_budget_secs
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                _ => {
                    emit_command_output(
                        app,
                        "Usage: /objective context [status|list|max|balanced|fast]",
                    );
                    return Ok(CommandResult::Handled);
                }
            }
        }

        if cmd == "profile" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            match sub.as_str() {
                "status" | "show" => {
                    let p = load_objective_profile()?;
                    let mut out = String::new();
                    out.push_str("Objective profile\n");
                    out.push_str("-----------------\n");
                    let _ = writeln!(out, "profile_id: {}", p.profile_id);
                    let _ = writeln!(out, "operator_hint: {}", p.operator_hint);
                    let _ = writeln!(out, "default_shell: {}", p.default_shell);
                    let _ = writeln!(out, "memory_backend: {}", p.memory_backend);
                    let _ = writeln!(out, "specialization_note: {}", p.specialization_note);
                    if !p.preferred_repos.is_empty() {
                        out.push_str("preferred_repos:\n");
                        for repo in p.preferred_repos {
                            let _ = writeln!(out, "- {}", repo);
                        }
                    }
                    if !p.preferred_languages.is_empty() {
                        out.push_str("preferred_languages:\n");
                        for lang in p.preferred_languages {
                            let _ = writeln!(out, "- {}", lang);
                        }
                    }
                    emit_command_output(app, out.trim_end());
                    return Ok(CommandResult::Handled);
                }
                "list" => {
                    emit_command_output(
                        app,
                        "Objective profile presets:\n- repo-general: generalized defaults for any operator/repo\n- sheawinkler: specialized ContextLattice+zsh profile\n- operator-custom: generated when using `/objective profile set <name>`",
                    );
                    return Ok(CommandResult::Handled);
                }
                "general" | "repo-general" | "reset" => {
                    let profile = reset_objective_profile_generalized()?;
                    emit_command_output(
                        app,
                        format!(
                            "Objective profile reset to generalized defaults.\nprofile_id={} memory_backend={} shell={}",
                            profile.profile_id, profile.memory_backend, profile.default_shell
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                "me" | "sheawinkler" => {
                    let profile = set_objective_profile(objective_profile_specialized_for(
                        std::env::var("USER")
                            .unwrap_or_else(|_| "sheawinkler".to_string())
                            .as_str(),
                    ))?;
                    emit_command_output(
                        app,
                        format!(
                            "Objective profile specialized for operator.\nprofile_id={} memory_backend={} shell={}",
                            profile.profile_id, profile.memory_backend, profile.default_shell
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                "set" => {
                    let Some(name) = args.get(2) else {
                        emit_command_output(
                            app,
                            "Usage: /objective profile set <name> (or use /objective profile me|general)",
                        );
                        return Ok(CommandResult::Handled);
                    };
                    let profile = set_objective_profile(objective_profile_specialized_for(name))?;
                    emit_command_output(
                        app,
                        format!(
                            "Objective profile set.\nprofile_id={} operator_hint={} shell={} memory_backend={}",
                            profile.profile_id,
                            profile.operator_hint,
                            profile.default_shell,
                            profile.memory_backend
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                _ => {
                    emit_command_output(
                        app,
                        "Usage: /objective profile [status|list|general|me|set <id>]",
                    );
                    return Ok(CommandResult::Handled);
                }
            }
        }

        if cmd == "simulator" || cmd == "simulation" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "status" || sub == "show" {
                let p = load_objective_simulation_policy()?;
                emit_command_output(
                    app,
                    format!(
                        "Objective simulation policy\nmode={}\nrequire_shadow_pass={}\nmin_shadow_samples={}\nrequire_replay_validation={}\nmax_live_capital_fraction={:.4}\nupdated_at={}",
                        p.mode,
                        p.require_shadow_pass,
                        p.min_shadow_samples,
                        p.require_replay_validation,
                        p.max_live_capital_fraction,
                        p.updated_at
                    ),
                );
                return Ok(CommandResult::Handled);
            }
            if !matches!(sub.as_str(), "balanced" | "strict" | "aggressive") {
                emit_command_output(
                    app,
                    "Usage: /objective simulator [status|balanced|strict|aggressive]",
                );
                return Ok(CommandResult::Handled);
            }
            let p = set_objective_simulation_mode(&sub)?;
            emit_command_output(
                app,
                format!(
                    "Objective simulation policy updated.\nmode={} shadow_pass={} replay_validation={} max_live_capital_fraction={:.4}",
                    p.mode,
                    p.require_shadow_pass,
                    p.require_replay_validation,
                    p.max_live_capital_fraction
                ),
            );
            return Ok(CommandResult::Handled);
        }

        if cmd == "ensemble" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "status" || sub == "show" {
                let p = load_objective_ensemble_policy()?;
                emit_command_output(
                    app,
                    format!(
                        "Objective ensemble policy\nmode={}\narbitration={}\nmin_voters={}\nrequire_disagreement_explainer={}\nallow_fast_path_single_model={}\nupdated_at={}",
                        p.mode,
                        p.arbitration,
                        p.min_voters,
                        p.require_disagreement_explainer,
                        p.allow_fast_path_single_model,
                        p.updated_at
                    ),
                );
                return Ok(CommandResult::Handled);
            }
            if !matches!(sub.as_str(), "committee" | "single" | "debate") {
                emit_command_output(
                    app,
                    "Usage: /objective ensemble [status|committee|single|debate]",
                );
                return Ok(CommandResult::Handled);
            }
            let p = set_objective_ensemble_mode(&sub)?;
            emit_command_output(
                app,
                format!(
                    "Objective ensemble policy updated.\nmode={} arbitration={} min_voters={} disagreement_explainer={}",
                    p.mode, p.arbitration, p.min_voters, p.require_disagreement_explainer
                ),
            );
            return Ok(CommandResult::Handled);
        }

        if cmd == "ledger" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "clear" {
                clear_objective_learning_ledger()?;
                emit_command_output(app, "Objective learning ledger cleared.");
                return Ok(CommandResult::Handled);
            }
            let ledger = load_objective_learning_ledger()?;
            if sub == "status" || sub == "show" {
                let last = ledger
                    .entries
                    .last()
                    .map(|v| format!("{} {} {}", v.recorded_at, v.objective_state, v.decision))
                    .unwrap_or_else(|| "none".to_string());
                emit_command_output(
                    app,
                    format!(
                        "Objective learning ledger\nentries={}\nupdated_at={}\nlast_entry={}",
                        ledger.entries.len(),
                        ledger.updated_at,
                        last
                    ),
                );
                return Ok(CommandResult::Handled);
            }
            if sub == "tail" {
                let n = args
                    .get(2)
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(8)
                    .clamp(1, 64);
                let mut out = String::new();
                out.push_str("Objective learning ledger tail\n");
                out.push_str("-----------------------------\n");
                let start = ledger.entries.len().saturating_sub(n);
                for row in &ledger.entries[start..] {
                    let _ = writeln!(
                        out,
                        "- {} id={} state={} decision={} notes={}",
                        row.recorded_at,
                        row.objective_id,
                        row.objective_state,
                        row.decision,
                        row.notes
                    );
                }
                if ledger.entries.is_empty() {
                    out.push_str("(empty)\n");
                }
                emit_command_output(app, out.trim_end());
                return Ok(CommandResult::Handled);
            }
            emit_command_output(app, "Usage: /objective ledger [status|tail [n]|clear]");
            return Ok(CommandResult::Handled);
        }

        if cmd == "dag" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            if sub == "rebuild" || sub == "build" {
                let dag = build_objective_dag_from_contract()?;
                emit_command_output(
                    app,
                    format!(
                        "Objective DAG rebuilt.\nobjective_id={}\nnodes={}\nauto_resume_checkpoint={}",
                        dag.objective_id,
                        dag.nodes.len(),
                        dag.auto_resume_checkpoint
                    ),
                );
                return Ok(CommandResult::Handled);
            }
            if sub == "clear" {
                clear_objective_dag()?;
                emit_command_output(app, "Objective DAG cleared.");
                return Ok(CommandResult::Handled);
            }
            let dag = load_objective_dag()?;
            let mut out = String::new();
            out.push_str("Objective DAG\n");
            out.push_str("-------------\n");
            let _ = writeln!(out, "objective_id: {}", dag.objective_id);
            let _ = writeln!(out, "updated_at: {}", dag.updated_at);
            let _ = writeln!(
                out,
                "auto_resume_checkpoint: {}",
                dag.auto_resume_checkpoint
            );
            if dag.nodes.is_empty() {
                out.push_str("nodes: (empty)\n");
            } else {
                for node in dag.nodes {
                    let _ = writeln!(
                        out,
                        "- {} [{}] depends_on=[{}] rollback={}",
                        node.id,
                        node.status,
                        node.depends_on.join(","),
                        node.rollback
                    );
                    let _ = writeln!(out, "  title: {}", node.title);
                }
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "eval" {
            let sub = args
                .get(1)
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "status".to_string());
            let trend = load_objective_eval_trend()?;
            if sub == "tail" {
                let n = args
                    .get(2)
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(12)
                    .clamp(1, 100);
                let start = trend.samples.len().saturating_sub(n);
                let mut out = String::new();
                out.push_str("Objective eval trend tail\n");
                out.push_str("------------------------\n");
                for sample in &trend.samples[start..] {
                    let _ = writeln!(
                        out,
                        "- {} id={} state={} score={:.3} note={}",
                        sample.recorded_at,
                        sample.objective_id,
                        sample.objective_state,
                        sample.score,
                        sample.note
                    );
                }
                if trend.samples.is_empty() {
                    out.push_str("(empty)\n");
                }
                emit_command_output(app, out.trim_end());
                return Ok(CommandResult::Handled);
            }
            let latest = trend.samples.last().map(|s| s.score).unwrap_or(0.0);
            let avg = if trend.samples.is_empty() {
                0.0
            } else {
                trend.samples.iter().map(|s| s.score).sum::<f64>() / trend.samples.len() as f64
            };
            emit_command_output(
                app,
                format!(
                    "Objective eval trend\nsamples={}\nlatest_score={:.3}\navg_score={:.3}\nupdated_at={}",
                    trend.samples.len(),
                    latest,
                    avg,
                    trend.updated_at
                ),
            );
            return Ok(CommandResult::Handled);
        }

        if cmd == "verify" {
            let Some(contract) = load_objective_contract()? else {
                emit_command_output(
                    app,
                    "No objective contract. Set one with `/objective <text>` before verify.",
                );
                return Ok(CommandResult::Handled);
            };
            let trend = load_objective_eval_trend()?;
            let ledger = load_objective_learning_ledger()?;
            let latest = trend.samples.last().map(|s| s.score).unwrap_or(0.0);
            let prev = if trend.samples.len() >= 2 {
                trend
                    .samples
                    .get(trend.samples.len().saturating_sub(2))
                    .map(|s| s.score)
                    .unwrap_or(latest)
            } else {
                latest
            };
            let delta = latest - prev;
            let avg = if trend.samples.is_empty() {
                0.0
            } else {
                trend.samples.iter().map(|s| s.score).sum::<f64>() / trend.samples.len() as f64
            };
            let ledger_tail = ledger.entries.last();
            let last_ledger_state = ledger_tail
                .map(|entry| entry.objective_state.as_str())
                .unwrap_or("unknown");
            let has_contract = !contract.id.trim().is_empty();
            let outcome = if !has_contract {
                "unproven"
            } else if latest >= 0.75 && delta >= -0.01 {
                "advancing"
            } else if latest <= 0.35 || delta < -0.05 {
                "regressing"
            } else if trend.samples.len() < 2 {
                "unproven"
            } else {
                "flat"
            };
            let mut evidence_files: Vec<String> = Vec::new();
            let mut verified_existing = 0usize;
            if let Some(last_assistant) = app
                .messages
                .iter()
                .rev()
                .find(|m| m.role == hermes_core::MessageRole::Assistant)
                .and_then(|m| m.content.as_deref())
            {
                if let Ok(path_re) = Regex::new(r"path=([^\s]+)") {
                    for cap in path_re.captures_iter(last_assistant) {
                        if let Some(path) = cap.get(1).map(|m| m.as_str().trim()) {
                            if path.is_empty() {
                                continue;
                            }
                            if !evidence_files.iter().any(|v| v == path) {
                                let exists = Path::new(path).exists();
                                if exists {
                                    verified_existing += 1;
                                }
                                evidence_files.push(path.to_string());
                            }
                        }
                    }
                }
            }
            let mut out = String::new();
            out.push_str("Objective outcome verifier\n");
            out.push_str("-------------------------\n");
            let _ = writeln!(out, "objective_id={}", contract.id);
            let _ = writeln!(out, "objective_state={}", outcome);
            let _ = writeln!(out, "latest_score={:.3}", latest);
            let _ = writeln!(out, "delta_vs_prev={:+.3}", delta);
            let _ = writeln!(out, "avg_score={:.3}", avg);
            let _ = writeln!(out, "trend_samples={}", trend.samples.len());
            let _ = writeln!(out, "ledger_entries={}", ledger.entries.len());
            let _ = writeln!(out, "ledger_last_state={}", last_ledger_state);
            let _ = writeln!(out, "verified_files_present={}", verified_existing);
            let _ = writeln!(out, "verified_files_total={}", evidence_files.len());
            if evidence_files.is_empty() {
                let _ = writeln!(
                    out,
                    "note=no PATCH_VERIFIED path markers found in last assistant turn; file verification is unproven."
                );
            } else {
                out.push_str("verified_paths:\n");
                for path in evidence_files.iter().take(12) {
                    let _ = writeln!(
                        out,
                        "- {} exists_now={}",
                        path,
                        yes_no(Path::new(path).exists())
                    );
                }
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "status" || cmd == "show" {
            let mut out = String::new();
            match app.session_objective.as_deref() {
                Some(v) => {
                    let _ = writeln!(out, "Current objective:\n{}", v);
                }
                None => {
                    let _ = writeln!(out, "No session objective set.");
                }
            }
            if let Some(contract) = load_objective_contract()? {
                let _ = writeln!(out, "\nObjective contract");
                let _ = writeln!(out, "------------------");
                let _ = writeln!(out, "{}", summarize_objective_contract(&contract));
                let _ = writeln!(
                    out,
                    "status_reason: {}",
                    if contract.status_reason.trim().is_empty() {
                        "(none)"
                    } else {
                        contract.status_reason.trim()
                    }
                );
                if !contract.behavior_directives.is_empty() {
                    let _ = writeln!(
                        out,
                        "behavior_directives: {}",
                        contract.behavior_directives.join(" | ")
                    );
                }
            } else {
                let _ = writeln!(out, "\nNo persisted objective contract yet.");
            }
            if let Ok(profile) = load_objective_profile() {
                let _ = writeln!(
                    out,
                    "\nObjective profile\n-----------------\nprofile_id: {}\noperator_hint: {}\nmemory_backend: {}\ndefault_shell: {}",
                    profile.profile_id, profile.operator_hint, profile.memory_backend, profile.default_shell
                );
            }
            if let Ok(ctx_policy) = load_contextlattice_policy() {
                let _ = writeln!(
                    out,
                    "\nContextLattice policy\n---------------------\nmode_hint: {}\npreflight_required: {}\nretrieval_debug: {}\nreadback_required: {}\ndeep_retries: {:?}\nregular_retries: {:?}",
                    ctx_policy.preferred_retrieval_mode,
                    ctx_policy.preflight_required,
                    ctx_policy.include_retrieval_debug_for_execution,
                    ctx_policy.readback_verification_required,
                    ctx_policy.deep_retry_budget_secs,
                    ctx_policy.regular_retry_budget_secs
                );
            }
            if let Ok(sim) = load_objective_simulation_policy() {
                let _ = writeln!(
                    out,
                    "\nSimulation policy\n-----------------\nmode: {} (shadow_pass={} replay_validation={} cap={:.4})",
                    sim.mode, sim.require_shadow_pass, sim.require_replay_validation, sim.max_live_capital_fraction
                );
            }
            if let Ok(ensemble) = load_objective_ensemble_policy() {
                let _ = writeln!(
                    out,
                    "\nEnsemble policy\n---------------\nmode: {} (arbitration={} min_voters={})",
                    ensemble.mode, ensemble.arbitration, ensemble.min_voters
                );
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "plan" {
            let Some(contract) = load_objective_contract()? else {
                emit_command_output(
                    app,
                    "No objective contract. Set one with `/objective <text>`.",
                );
                return Ok(CommandResult::Handled);
            };
            let mut out = String::new();
            out.push_str("Objective horizon plan\n");
            out.push_str("----------------------\n");
            for horizon in contract.horizons {
                let _ = writeln!(out, "- {}:", horizon.horizon);
                for goal in horizon.goals {
                    let _ = writeln!(out, "  - {}", goal);
                }
            }
            let terms = utility_terms_from_contract()?;
            if !terms.is_empty() {
                let mut rows: Vec<(String, f64)> = terms.into_iter().collect();
                rows.sort_by(|a, b| b.1.total_cmp(&a.1));
                out.push_str("\nUtility weights:\n");
                for (name, weight) in rows {
                    let _ = writeln!(out, "- {}: {:.2}", name, weight);
                }
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "constraints" {
            let Some(contract) = load_objective_contract()? else {
                emit_command_output(
                    app,
                    "No objective contract. Set one with `/objective <text>`.",
                );
                return Ok(CommandResult::Handled);
            };
            let mut out = String::new();
            out.push_str("Objective hard constraints\n");
            out.push_str("--------------------------\n");
            for c in contract.utility.hard_constraints {
                let _ = writeln!(out, "- {}", c.expression);
            }
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }

        if cmd == "counterfactual" {
            if args.len() < 2 {
                emit_command_output(
                    app,
                    "Usage: /objective counterfactual <scenario> | <expected_delta>",
                );
                return Ok(CommandResult::Handled);
            }
            let joined = args[1..].join(" ");
            let (scenario, expected_delta) = joined
                .split_once('|')
                .map(|(a, b)| (a.trim(), b.trim()))
                .unwrap_or((joined.trim(), "impact not specified"));
            if scenario.is_empty() {
                emit_command_output(
                    app,
                    "Counterfactual scenario cannot be empty. Use: /objective counterfactual <scenario> | <expected_delta>",
                );
                return Ok(CommandResult::Handled);
            }
            let updated = append_counterfactual(scenario, expected_delta)?;
            emit_command_output(
                app,
                format!(
                    "Counterfactual saved (journal entries={}).",
                    updated.counterfactual_journal.len()
                ),
            );
            return Ok(CommandResult::Handled);
        }
    }

    if args.is_empty() {
        let msg = match app.session_objective.as_deref() {
            Some(v) => format!(
                "Current objective:\n{}\n\nUse `/objective clear` to remove, `/objective <text>` to replace, or `/objective status` for contract details.",
                v
            ),
            None => format!("No objective set.\n{}", objective_usage),
        };
        emit_command_output(app, msg);
        return Ok(CommandResult::Handled);
    }

    let first = args[0].trim();
    if first.eq_ignore_ascii_case("clear")
        || first.eq_ignore_ascii_case("off")
        || first.eq_ignore_ascii_case("none")
        || first.eq_ignore_ascii_case("reset")
    {
        let previous_id = load_objective_contract()?
            .map(|c| c.id)
            .unwrap_or_else(|| "none".to_string());
        app.set_session_objective(None);
        clear_objective_contract()?;
        let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
            recorded_at: String::new(),
            objective_id: previous_id,
            objective_state: "cleared".to_string(),
            decision: "objective_clear".to_string(),
            evidence_files: vec![],
            evidence_commands: vec!["/objective clear".to_string()],
            notes: "Objective contract cleared by operator command.".to_string(),
        });
        emit_command_output(app, "Session objective cleared.");
        return Ok(CommandResult::Handled);
    }

    let objective = args.join(" ").trim().to_string();
    if objective.is_empty() {
        emit_command_output(app, objective_usage);
        return Ok(CommandResult::Handled);
    }
    let objective_lc = objective.to_ascii_lowercase();
    let trading_sensitive = [
        "trading", "sol", "kraken", "wallet", "pnl", "strategy", "market",
    ]
    .iter()
    .any(|needle| objective_lc.contains(needle));
    let contract = upsert_objective_contract(&objective, trading_sensitive)?;
    let _ = build_objective_dag_from_contract();
    let lifecycle = canonical_objective_lifecycle_status(&contract.lifecycle_status);
    if objective_lifecycle_is_active(&lifecycle) {
        app.set_session_objective(Some(objective.clone()));
    } else {
        app.set_session_objective(None);
    }
    let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
        recorded_at: String::new(),
        objective_id: contract.id.clone(),
        objective_state: lifecycle.clone(),
        decision: "objective_set".to_string(),
        evidence_files: vec!["alpha/objective_contract.json".to_string()],
        evidence_commands: vec!["/objective <text>".to_string()],
        notes: if trading_sensitive {
            "Trading-sensitive objective configured.".to_string()
        } else {
            "General objective configured.".to_string()
        },
    });
    emit_command_output(
        app,
        format!(
            "Session objective set:\n{}\n\nObjective contract persisted:\n{}\n\nlifecycle_status={}\nbehavior_mode={}\nobjective_injected={}\n\nThis objective is now injected as system context for future turns when lifecycle is active.",
            objective,
            summarize_objective_contract(&contract),
            lifecycle,
            canonical_objective_behavior_mode(&contract.behavior_mode),
            yes_no(objective_lifecycle_is_active(&lifecycle))
        ),
    );
    Ok(CommandResult::Handled)
}
