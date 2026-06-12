//! Typed command dispatch for `apply_command_result`.
//!
//! Replaces the 45-variant `match` in `Gateway::apply_command_result` with
//! eight executor families, each covering a coherent set of variants.  The
//! top-level `apply_command` function routes by family and delegates.
//!
//! Family taxonomy (mirrors `GatewayCommandResult` variant names):
//!
//! | Family          | Variants                                                     |
//! |-----------------|--------------------------------------------------------------|
//! | `reply_only`    | Reply / ShowHelp / Unknown / Noop / ShowInsights / CheckUpdate / ReloadMcp |
//! | `session`       | ResetSession / Rollback / Undo / Retry / CompressContext / ListSessions / SwitchSession |
//! | `runtime_switch`| SwitchModel / SwitchProvider / SwitchProfile / SwitchBranch / SwitchPersonality / SetHome / SwitchFast |
//! | `mode_toggle`   | ToggleVerbose / ToggleYolo / ToggleReasoning / ShowUsage / ShowStatus / ShowBudget |
//! | `admin`         | ApproveUser / DenyUser / ResolveCommandApproval              |
//! | `background`    | StopAgent / BackgroundTask / BtwTask                         |
//! | `tool`          | ListTools / EnableTool / DisableTool                         |
//! | `curator`       | CuratorStatus/Run/Pause/Resume/Pin/Unpin/Archive/Restore/ListArchived |

use crate::commands::GatewayCommandResult;
use crate::gateway::{Gateway, IncomingMessage};
use hermes_core::errors::GatewayError;
use hermes_core::types::MessageRole;
use tracing::info;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatch `result` to the appropriate executor family.
///
/// Returns `Ok(true)` if the command was handled (the caller should return
/// early), `Ok(false)` if the command was not recognised by any family.
pub(crate) async fn apply_command(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    match result {
        // ── reply-only ────────────────────────────────────────────────────
        GatewayCommandResult::Reply(text)
        | GatewayCommandResult::ShowHelp(text)
        | GatewayCommandResult::Unknown(text)
        | GatewayCommandResult::ShowInsights(text) => {
            gw.send_incoming_reply(incoming, &text, None).await?;
            Ok(true)
        }
        GatewayCommandResult::Noop => Ok(true),
        GatewayCommandResult::CheckUpdate => {
            let version =
                std::env::var("HERMES_LATEST_VERSION").unwrap_or_else(|_| "latest".to_string());
            gw.send_update_notification(&incoming.platform, &incoming.chat_id, &version)
                .await?;
            Ok(true)
        }
        GatewayCommandResult::ReloadMcp => {
            let mut generation = gw.router.mcp_reload_generation.write().await;
            *generation += 1;
            let current = *generation;
            drop(generation);
            gw.send_message(
                &incoming.platform,
                &incoming.chat_id,
                &format!("🔄 MCP registry reloaded (generation {}).", current),
                None,
            )
            .await?;
            Ok(true)
        }
        GatewayCommandResult::EvolveStatus => {
            let reply = gw.execute_evolve_status();
            gw.send_incoming_reply(incoming, &reply, None).await?;
            Ok(true)
        }

        // ── session ───────────────────────────────────────────────────────
        result @ (GatewayCommandResult::ResetSession(_)
        | GatewayCommandResult::Rollback { .. }
        | GatewayCommandResult::Undo
        | GatewayCommandResult::Retry
        | GatewayCommandResult::CompressContext(_)
        | GatewayCommandResult::ListSessions
        | GatewayCommandResult::SwitchSession { .. }) => {
            apply_session(gw, incoming, session_key, result).await
        }

        // ── runtime switch ────────────────────────────────────────────────
        result @ (GatewayCommandResult::SwitchModel { .. }
        | GatewayCommandResult::SwitchProvider { .. }
        | GatewayCommandResult::SwitchProfile { .. }
        | GatewayCommandResult::SwitchBranch { .. }
        | GatewayCommandResult::SwitchPersonality { .. }
        | GatewayCommandResult::SetHome { .. }
        | GatewayCommandResult::SwitchFast { .. }) => {
            apply_runtime_switch(gw, incoming, session_key, result).await
        }

        // ── mode toggle ───────────────────────────────────────────────────
        result @ (GatewayCommandResult::ToggleVerbose(_)
        | GatewayCommandResult::ToggleYolo(_)
        | GatewayCommandResult::ToggleReasoning(_)
        | GatewayCommandResult::ShowUsage(_)
        | GatewayCommandResult::ShowStatus(_)
        | GatewayCommandResult::ShowBudget { .. }) => {
            apply_mode_toggle(gw, incoming, session_key, result).await
        }

        // ── admin ─────────────────────────────────────────────────────────
        result @ (GatewayCommandResult::ApproveUser { .. }
        | GatewayCommandResult::DenyUser { .. }
        | GatewayCommandResult::ResolveCommandApproval { .. }) => {
            apply_admin(gw, incoming, session_key, result).await
        }

        // ── background / stop ─────────────────────────────────────────────
        result @ (GatewayCommandResult::StopAgent(_)
        | GatewayCommandResult::BackgroundTask { .. }
        | GatewayCommandResult::BtwTask { .. }) => {
            apply_background(gw, incoming, session_key, result).await
        }

        GatewayCommandResult::PlanMode { args } => {
            apply_plan_mode(gw, incoming, session_key, args).await
        }

        // ── tool ──────────────────────────────────────────────────────────
        result @ (GatewayCommandResult::ListTools { .. }
        | GatewayCommandResult::EnableTool { .. }
        | GatewayCommandResult::DisableTool { .. }) => apply_tool(gw, incoming, result).await,

        // ── curator ───────────────────────────────────────────────────────
        result @ (GatewayCommandResult::CuratorStatus
        | GatewayCommandResult::CuratorRun { .. }
        | GatewayCommandResult::CuratorPause
        | GatewayCommandResult::CuratorResume
        | GatewayCommandResult::CuratorPin { .. }
        | GatewayCommandResult::CuratorUnpin { .. }
        | GatewayCommandResult::CuratorArchive { .. }
        | GatewayCommandResult::CuratorRestore { .. }
        | GatewayCommandResult::CuratorListArchived) => {
            apply_curator(gw, incoming, session_key, result).await
        }
    }
}

// ---------------------------------------------------------------------------
// Plan mode
// ---------------------------------------------------------------------------

async fn apply_plan_mode(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    args: String,
) -> Result<bool, GatewayError> {
    if let Some(handler) = gw.plan_mode_slash_handler.read().await.clone() {
        handler(incoming.clone(), session_key.to_string(), args).await?;
    } else {
        gw.send_incoming_reply(
            incoming,
            "Plan mode is not available on this gateway runtime.",
            None,
        )
        .await?;
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Session family
// ---------------------------------------------------------------------------

async fn apply_session(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    match result {
        GatewayCommandResult::ResetSession(reply) => {
            gw.emit_hook_event(
                "session:end",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key
                }),
            )
            .await;
            gw.teardown_session_key(session_key, "reset").await;
            gw.session.session_manager.reset_session(session_key).await;
            gw.clear_session_boundary_security_state(session_key).await;
            gw.emit_hook_event(
                "session:reset",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key
                }),
            )
            .await;
            gw.send_incoming_reply(incoming, &reply, None).await?;
            Ok(true)
        }
        GatewayCommandResult::CompressContext(_) => {
            let outcome = gw.compress_context(session_key, 24).await;
            let mut reply = format!(
                "📦 Context compressed. Removed {} old messages.",
                outcome.removed_messages
            );
            if let Some(warning) = outcome.summary_warning {
                reply.push_str("\n\n");
                reply.push_str(&warning);
            }
            gw.send_incoming_reply(incoming, &reply, None).await?;
            Ok(true)
        }
        GatewayCommandResult::Rollback { steps } => {
            let mut removed = 0usize;
            for _ in 0..steps {
                if gw
                    .session
                    .session_manager
                    .pop_last_message(session_key)
                    .await
                    .is_some()
                {
                    removed += 1;
                } else {
                    break;
                }
            }
            gw.send_message(
                &incoming.platform,
                &incoming.chat_id,
                &format!("↪️ Rolled back {} message(s).", removed),
                None,
            )
            .await?;
            Ok(true)
        }
        GatewayCommandResult::Undo => {
            let mut removed = 0usize;
            if let Some(last) = gw
                .session
                .session_manager
                .pop_last_message(session_key)
                .await
            {
                removed += 1;
                if last.role == MessageRole::Assistant {
                    if let Some(prev) = gw
                        .session
                        .session_manager
                        .pop_last_message(session_key)
                        .await
                    {
                        if prev.role == MessageRole::User {
                            removed += 1;
                        }
                    }
                }
            }
            let reply = if removed == 0 {
                "Nothing to undo.".to_string()
            } else {
                format!("↩️ Removed {} message(s) from current session.", removed)
            };
            gw.send_incoming_reply(incoming, &reply, None).await?;
            Ok(true)
        }
        GatewayCommandResult::Retry => {
            let mut messages = gw.session.session_manager.get_messages(session_key).await;
            if matches!(
                messages.last().map(|m| m.role),
                Some(MessageRole::Assistant)
            ) {
                messages.pop();
            }
            if messages.is_empty() {
                gw.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "No previous message to retry.",
                    None,
                )
                .await?;
                return Ok(true);
            }
            gw.session
                .session_manager
                .replace_messages(session_key, messages)
                .await;
            let snapshot = gw
                .session
                .session_manager
                .snapshot_messages(session_key)
                .await;
            let route_id = Gateway::route_correlation_id(incoming, session_key);
            gw.route_non_streaming(incoming, snapshot, session_key, &route_id)
                .await?;
            Ok(true)
        }
        GatewayCommandResult::ListSessions => {
            let sessions = gw
                .session
                .session_manager
                .get_user_sessions(&incoming.user_id)
                .await;
            let text = if sessions.is_empty() {
                "📚 No sessions found for your user.".to_string()
            } else {
                let mut out = String::from("📚 **Your sessions:**\n\n");
                for s in sessions {
                    let key = gw.session.session_manager.compose_session_key(
                        &s.platform,
                        &s.chat_id,
                        &s.user_id,
                    );
                    out.push_str(&format!(
                        "• `{}` — {} messages, platform `{}` (id `{}`)\n",
                        key,
                        s.messages.len(),
                        s.platform,
                        s.id
                    ));
                }
                out.push_str("\nUse `/sessions <key or id>` to switch.");
                out
            };
            gw.send_incoming_reply(incoming, &text, None).await?;
            Ok(true)
        }
        GatewayCommandResult::SwitchSession { session_id } => {
            let sessions = gw
                .session
                .session_manager
                .get_user_sessions(&incoming.user_id)
                .await;
            let matched = sessions.iter().find(|s| {
                let key = gw.session.session_manager.compose_session_key(
                    &s.platform,
                    &s.chat_id,
                    &s.user_id,
                );
                key == session_id || s.id == session_id
            });
            let msg = if let Some(target) = matched {
                let copied = gw
                    .session
                    .session_manager
                    .replace_messages(session_key, target.messages.as_ref().clone())
                    .await;
                if copied {
                    gw.clear_session_boundary_security_state(session_key).await;
                    format!(
                        "🔁 Switched to session `{}`.\nLoaded {} message(s) into this chat context.",
                        session_id,
                        target.messages.len()
                    )
                } else {
                    format!(
                        "❌ Could not switch to `{}` because the current chat session key is missing.",
                        session_id
                    )
                }
            } else {
                format!(
                    "❌ No session matching `{}` for your user. Try `/sessions` to list keys.",
                    session_id
                )
            };
            gw.send_incoming_reply(incoming, &msg, None).await?;
            Ok(true)
        }
        _ => unreachable!("apply_session called with non-session variant"),
    }
}

// ---------------------------------------------------------------------------
// Runtime-switch family
// ---------------------------------------------------------------------------

async fn apply_runtime_switch(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    let reply = match result {
        GatewayCommandResult::SwitchModel { model, reply } => {
            let mut states = gw.session.runtime_state.write().await;
            states.entry(session_key.to_string()).or_default().model = Some(model);
            reply
        }
        GatewayCommandResult::SwitchPersonality { name, reply } => {
            let mut states = gw.session.runtime_state.write().await;
            states
                .entry(session_key.to_string())
                .or_default()
                .personality = Some(name);
            reply
        }
        GatewayCommandResult::SwitchProvider { provider, reply } => {
            let mut states = gw.session.runtime_state.write().await;
            states.entry(session_key.to_string()).or_default().provider = Some(provider);
            reply
        }
        GatewayCommandResult::SwitchProfile { profile, reply } => {
            let mut states = gw.session.runtime_state.write().await;
            states.entry(session_key.to_string()).or_default().profile = Some(profile);
            reply
        }
        GatewayCommandResult::SwitchBranch { branch } => match branch {
            Some(name) => {
                let mut states = gw.session.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().branch = Some(name.clone());
                format!("🌿 Branch context switched to: {}", name)
            }
            None => {
                let branch = gw
                    .session
                    .runtime_state
                    .read()
                    .await
                    .get(session_key)
                    .and_then(|s| s.branch.clone())
                    .unwrap_or_else(|| "main".to_string());
                format!("🌿 Current branch context: {}", branch)
            }
        },
        GatewayCommandResult::SwitchFast {
            service_tier,
            reply,
        } => {
            let mut states = gw.session.runtime_state.write().await;
            states
                .entry(session_key.to_string())
                .or_default()
                .service_tier = service_tier;
            reply
        }
        GatewayCommandResult::SetHome { path, reply } => {
            let target = std::path::Path::new(&path);
            if target.exists() && target.is_dir() {
                let mut states = gw.session.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().home = Some(path);
                reply
            } else {
                format!("❌ Path not found or not a directory: {}", path)
            }
        }
        _ => unreachable!("apply_runtime_switch called with non-runtime-switch variant"),
    };
    gw.send_incoming_reply(incoming, &reply, None).await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Mode-toggle family
// ---------------------------------------------------------------------------

async fn apply_mode_toggle(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    match result {
        GatewayCommandResult::ToggleVerbose(_) => {
            let reply = gw.apply_verbose_command(incoming, session_key).await?;
            gw.send_incoming_reply(incoming, &reply, None).await?;
        }
        GatewayCommandResult::ToggleYolo(_) => {
            let mut states = gw.session.runtime_state.write().await;
            let state = states.entry(session_key.to_string()).or_default();
            state.yolo = !state.yolo;
            if state.yolo {
                hermes_tools::approval::enable_session_yolo(session_key);
            } else {
                hermes_tools::approval::disable_session_yolo(session_key);
            }
            let reply = format!("🤠 YOLO mode: {}", if state.yolo { "ON" } else { "OFF" });
            drop(states);
            gw.send_incoming_reply(incoming, &reply, None).await?;
        }
        GatewayCommandResult::ToggleReasoning(_) => {
            let mut states = gw.session.runtime_state.write().await;
            let state = states.entry(session_key.to_string()).or_default();
            state.reasoning = !state.reasoning;
            let reply = format!(
                "🧠 Reasoning visibility: {}",
                if state.reasoning { "ON" } else { "OFF" }
            );
            drop(states);
            gw.send_incoming_reply(incoming, &reply, None).await?;
        }
        GatewayCommandResult::ShowUsage(_) => {
            let text = gw.build_usage_text(session_key).await;
            gw.send_incoming_reply(incoming, &text, None).await?;
        }
        GatewayCommandResult::ShowStatus(_) => {
            let text = gw.build_status_text(session_key).await;
            gw.send_incoming_reply(incoming, &text, None).await?;
        }
        GatewayCommandResult::ShowBudget { new_budget } => {
            let mut states = gw.session.runtime_state.write().await;
            let state = states.entry(session_key.to_string()).or_default();
            let msg = match new_budget {
                Some(b) => {
                    state.budget = Some(b);
                    format!("💰 Usage budget set to {:.4}.", b)
                }
                None => match state.budget {
                    Some(b) => format!("💰 Current usage budget: {:.4}.", b),
                    None => {
                        "💰 No usage budget set. Use `/budget <amount>` to set one.".to_string()
                    }
                },
            };
            drop(states);
            gw.send_incoming_reply(incoming, &msg, None).await?;
        }
        _ => unreachable!("apply_mode_toggle called with non-mode-toggle variant"),
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Admin family
// ---------------------------------------------------------------------------

async fn apply_admin(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    match result {
        GatewayCommandResult::ApproveUser { user_id } => {
            let mut dm = gw.router.dm_manager.write().await;
            if !dm.is_admin(&incoming.user_id) {
                drop(dm);
                gw.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "🚫 /approve requires admin privileges.",
                    None,
                )
                .await?;
                return Ok(true);
            }
            dm.authorize_user(user_id.clone());
            drop(dm);
            gw.send_message(
                &incoming.platform,
                &incoming.chat_id,
                &format!("✅ User '{}' has been approved for DM access.", user_id),
                None,
            )
            .await?;
        }
        GatewayCommandResult::DenyUser { user_id } => {
            let mut dm = gw.router.dm_manager.write().await;
            if !dm.is_admin(&incoming.user_id) {
                drop(dm);
                gw.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "🚫 /deny requires admin privileges.",
                    None,
                )
                .await?;
                return Ok(true);
            }
            dm.deauthorize_user(&user_id);
            drop(dm);
            gw.send_message(
                &incoming.platform,
                &incoming.chat_id,
                &format!("⛔ User '{}' has been removed from DM allowlist.", user_id),
                None,
            )
            .await?;
        }
        GatewayCommandResult::ResolveCommandApproval {
            choice,
            resolve_all,
        } => {
            let count =
                hermes_tools::approval::resolve_gateway_approval(session_key, choice, resolve_all);
            let reply = if count == 0 {
                "No pending command approval for this session.".to_string()
            } else if choice == hermes_tools::approval::ApprovalChoice::Deny {
                if count == 1 {
                    "Denied pending command. The blocked agent will resume with a denial."
                        .to_string()
                } else {
                    format!("Denied {count} pending commands.")
                }
            } else if count == 1 {
                format!(
                    "Approved pending command with `{}` scope. Resuming.",
                    choice.as_str()
                )
            } else {
                format!(
                    "Approved {count} pending commands with `{}` scope.",
                    choice.as_str()
                )
            };
            gw.send_incoming_reply(incoming, &reply, None).await?;
        }
        _ => unreachable!("apply_admin called with non-admin variant"),
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Background / stop family
// ---------------------------------------------------------------------------

async fn apply_background(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    use crate::background::TaskStatus;

    match result {
        GatewayCommandResult::StopAgent(reply) => {
            let _ = gw.abort_active_route(session_key).await;
            for (task_id, status, _) in gw.router.background_tasks.list_tasks() {
                if status == TaskStatus::Running {
                    let _ = gw.router.background_tasks.cancel(&task_id);
                }
            }
            gw.send_incoming_reply(incoming, &reply, None).await?;
        }
        GatewayCommandResult::BackgroundTask { prompt } => {
            info!(
                platform = %incoming.platform,
                session_key = %session_key,
                "dispatching background task"
            );
            gw.handle_background_command(incoming, session_key, &prompt, false)
                .await?;
        }
        GatewayCommandResult::BtwTask { prompt } => {
            gw.handle_background_command(incoming, session_key, &prompt, true)
                .await?;
        }
        _ => unreachable!("apply_background called with non-background variant"),
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Tool family
// ---------------------------------------------------------------------------

async fn apply_tool(
    gw: &Gateway,
    incoming: &IncomingMessage,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    let text = match result {
        GatewayCommandResult::ListTools { filter } => {
            let suffix = match &filter {
                Some(f) => format!(" (filter: `{}`)", f),
                None => String::new(),
            };
            format!(
                "🔧 Tools{}.\nRegistered MCP tools are resolved at runtime after reload.",
                suffix
            )
        }
        GatewayCommandResult::EnableTool { name } => {
            format!(
                "✅ Tool enabled: `{}` (effective on next agent turn).",
                name
            )
        }
        GatewayCommandResult::DisableTool { name } => {
            format!(
                "⛔ Tool disabled: `{}` (effective on next agent turn).",
                name
            )
        }
        _ => unreachable!("apply_tool called with non-tool variant"),
    };
    gw.send_incoming_reply(incoming, &text, None).await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Curator family
// ---------------------------------------------------------------------------

async fn apply_curator(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    result: GatewayCommandResult,
) -> Result<bool, GatewayError> {
    match result {
        GatewayCommandResult::CuratorRun { dry_run } => {
            // Phase 1: auto transitions (fast, milliseconds)
            let reply = gw.execute_curator_run(dry_run);
            gw.send_incoming_reply(incoming, &reply, None).await?;

            // Phase 2: spawn background LLM review (30-120s)
            if !dry_run {
                gw.spawn_curator_llm_review(incoming, session_key).await;
            }
            Ok(true)
        }
        _ => {
            let reply = match result {
                GatewayCommandResult::CuratorStatus => gw.execute_curator_status(),
                GatewayCommandResult::CuratorPause => gw.execute_curator_pause_resume(true),
                GatewayCommandResult::CuratorResume => gw.execute_curator_pause_resume(false),
                GatewayCommandResult::CuratorPin { name } => {
                    gw.execute_curator_pin_unpin(&name, true)
                }
                GatewayCommandResult::CuratorUnpin { name } => {
                    gw.execute_curator_pin_unpin(&name, false)
                }
                GatewayCommandResult::CuratorArchive { name } => gw.execute_curator_archive(&name),
                GatewayCommandResult::CuratorRestore { name } => {
                    gw.execute_curator_restore(&name)
                }
                GatewayCommandResult::CuratorListArchived => gw.execute_curator_list_archived(),
                _ => unreachable!("apply_curator called with non-curator variant"),
            };
            gw.send_incoming_reply(incoming, &reply, None).await?;
            Ok(true)
        }
    }
}
