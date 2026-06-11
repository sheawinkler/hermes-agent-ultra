//! Staged inbound message pipeline extracted from `Gateway::route_message`.
//!
//! `route_inbound` is the single orchestrator. Each stage is a named
//! `pub(crate)` function so it can be unit-tested independently.
//!
//! Invariants that must be preserved:
//! - `try_fast_paths` (stop / clarify) **must** run before `acquire_session_serial`.
//!   The stop command interrupts an in-flight route; acquiring the serial first
//!   would deadlock because the in-flight route already holds it.
//! - `active_routes` insert/remove wraps the agent future exactly as in the
//!   original implementation so that `abort_active_route` stays race-free.

use std::sync::Arc;
use std::time::Instant;

use futures::future::{AbortHandle, Abortable};
use tracing::{debug, info, warn};

use crate::commands::{GatewayCommandResult, handle_command};
use crate::dm::DmDecision;
use crate::gateway::{Gateway, IncomingMessage, RouteTypingGuard, inbound_text_log_fields};
use crate::message_router::{DmAccessMode, PlatformAccessPolicy};
use crate::session::Session;
use crate::session_layer::SessionGuard;
use hermes_core::errors::GatewayError;
use hermes_core::traits::PlatformAdapter;
use hermes_core::types::Message;

/// Orchestrates the full inbound message pipeline. Called by `Gateway::route_message`.
pub(crate) async fn route_inbound(
    gw: &Gateway,
    incoming: &IncomingMessage,
) -> Result<(), GatewayError> {
    let route_start = Instant::now();

    // Update messaging session context so outbound helpers know which
    // platform/chat we're currently handling.
    if let Some(ctx) = gw.delivery.messaging_session.read().await.as_ref() {
        ctx.set(&incoming.platform, &incoming.chat_id);
    }

    let access_policy = gw.platform_access_policy(&incoming.platform).await;
    let is_slash_command = incoming.text.trim_start().starts_with('/');

    if evaluate_access_gate(access_policy.as_ref(), incoming, is_slash_command) {
        return Ok(());
    }
    if evaluate_dm_gate(gw, incoming, access_policy.as_ref()).await? {
        return Ok(());
    }

    let Some(session_key) = resolve_session_key(gw, incoming).await? else {
        return Ok(());
    };
    let route_id = Gateway::route_correlation_id(incoming, &session_key);

    // Fast paths MUST run before acquiring the session serial (see module doc).
    if try_fast_paths(gw, incoming, &session_key, is_slash_command).await? {
        return Ok(());
    }

    let session_guard = gw.session.lock_session(&session_key).await;

    if setup_session(gw, incoming, &session_guard.key, is_slash_command)
        .await?
        .is_none()
    {
        return Ok(());
    }

    let (typing_guard, reaction_adapter) = begin_route_ux(gw, incoming).await;

    let result = {
        let (messages, _prep_ms) =
            prepare_user_turn(gw, incoming, &session_guard.key, &route_id, route_start).await?;
        dispatch_agent_route(
            gw,
            incoming,
            &session_guard,
            &route_id,
            messages,
            route_start,
        )
        .await
    };

    typing_guard.finish().await;
    finalize_reaction(reaction_adapter, incoming, &result).await;

    result
}

/// Returns `true` if the message should be dropped by platform access policy.
///
/// Checks group-mode allow/deny and slash-command allowlist enforcement before
/// any session work is done, so unauthorized messages never touch session state.
pub(crate) fn evaluate_access_gate(
    policy: Option<&PlatformAccessPolicy>,
    incoming: &IncomingMessage,
    is_slash_command: bool,
) -> bool {
    use crate::message_router::GroupAccessMode;

    let Some(policy) = policy else { return false };

    if !incoming.is_dm {
        match policy.group_mode {
            GroupAccessMode::Disabled => {
                debug!(
                    platform = incoming.platform,
                    user_id = incoming.user_id,
                    "Group traffic denied by platform policy"
                );
                return true;
            }
            GroupAccessMode::Allowlist => {
                if !policy.is_user_allowed(&incoming.user_id, &incoming.role_ids) {
                    debug!(
                        platform = incoming.platform,
                        user_id = incoming.user_id,
                        "Group message denied: user not in allowlist"
                    );
                    return true;
                }
            }
            GroupAccessMode::Open => {}
        }
    }

    if is_slash_command
        && policy.slash_requires_allowlist
        && policy.has_allowlist()
        && !policy.is_user_allowed(&incoming.user_id, &incoming.role_ids)
    {
        debug!(
            platform = incoming.platform,
            user_id = incoming.user_id,
            "Slash command denied: user not in platform allowlist"
        );
        return true;
    }

    false
}

/// Returns `true` if the DM should be denied or handled by pairing flow.
///
/// `Ok(true)` means "stop here, the message was handled (or silently dropped)".
/// `Ok(false)` means "DM is authorized, continue".
pub(crate) async fn evaluate_dm_gate(
    gw: &Gateway,
    incoming: &IncomingMessage,
    policy: Option<&PlatformAccessPolicy>,
) -> Result<bool, GatewayError> {
    if !incoming.is_dm {
        return Ok(false);
    }

    let dm_mode = policy.map(|p| p.dm_mode).unwrap_or_else(|| {
        match incoming.platform.trim().to_ascii_lowercase().as_str() {
            "wecom" | "weixin" | "qqbot" => DmAccessMode::Open,
            _ => DmAccessMode::Pairing,
        }
    });

    if dm_mode == DmAccessMode::Disabled {
        debug!(
            user_id = incoming.user_id,
            platform = incoming.platform,
            "DM denied: platform dm_policy is disabled"
        );
        return Ok(true);
    }

    if dm_mode == DmAccessMode::Allowlist {
        let allowed =
            policy.is_some_and(|p| p.is_user_allowed(&incoming.user_id, &incoming.role_ids));
        if !allowed {
            debug!(
                user_id = incoming.user_id,
                platform = incoming.platform,
                "DM denied: user not in platform allowlist"
            );
            return Ok(true);
        }
        return Ok(false);
    }

    if dm_mode == DmAccessMode::Open {
        return Ok(false);
    }

    // Pairing mode: check pairing store first, then delegate to DmManager.
    let pair_approved = gw
        .router
        .pairing_store
        .lock()
        .ok()
        .is_some_and(|store| store.is_approved(&incoming.platform, &incoming.user_id));
    if pair_approved {
        debug!(
            user_id = incoming.user_id,
            platform = incoming.platform,
            "DM authorized by pairing store"
        );
        return Ok(false);
    }

    let dm_manager = gw.router.dm_manager.read().await;
    let decision = dm_manager
        .handle_dm(&incoming.user_id, &incoming.platform)
        .await;
    drop(dm_manager);

    match decision {
        DmDecision::Allow => Ok(false),
        DmDecision::Pair { message } => {
            let pair_msg = gw
                .router
                .pairing_store
                .lock()
                .ok()
                .and_then(|store| {
                    store
                        .generate_code(&incoming.platform, &incoming.user_id, &incoming.user_id)
                        .ok()
                        .flatten()
                })
                .map(|code| {
                    format!(
                        "Hi~ I don't recognize you yet!\n\nHere's your pairing code: `{code}`\n\nAsk the bot owner to run:\n`hermes pairing approve {} {code}`",
                        incoming.platform
                    )
                })
                .or(message);
            if let Some(msg) = pair_msg {
                warn!(
                    user_id = %incoming.user_id,
                    platform = %incoming.platform,
                    dm_mode = ?dm_mode,
                    "Sending DM pairing approval message"
                );
                gw.send_incoming_reply(incoming, &msg, None).await?;
            }
            Ok(true)
        }
        DmDecision::Deny => {
            debug!(
                user_id = incoming.user_id,
                platform = incoming.platform,
                "DM denied for unauthorized user"
            );
            Ok(true)
        }
    }
}

/// Resolves the session key and handles Telegram-specific routing edge cases.
///
/// Returns `None` if the message was fully handled by a Telegram topic command
/// or lobby reply (caller should return `Ok(())`).
pub(crate) async fn resolve_session_key(
    gw: &Gateway,
    incoming: &IncomingMessage,
) -> Result<Option<String>, GatewayError> {
    let is_dm = Some(incoming.is_dm);
    let session_key =
        crate::telegram_topic::compose_telegram_session_key(incoming).unwrap_or_else(|| {
            gw.session.session_manager.compose_session_key_with_dm(
                &incoming.platform,
                &incoming.chat_id,
                &incoming.user_id,
                is_dm,
            )
        });

    if let Some(reply) = crate::telegram_topic::try_handle_topic_command(incoming) {
        gw.send_incoming_reply(incoming, &reply, None).await?;
        return Ok(None);
    }

    if let Some(reply) = crate::telegram_topic::telegram_lobby_reply(incoming) {
        gw.send_incoming_reply(incoming, &reply, None).await?;
        return Ok(None);
    }

    Ok(Some(session_key))
}

/// Handles stop and clarify fast paths without acquiring the session serial.
///
/// Returns `true` if the message was fully handled (caller should return `Ok(())`).
pub(crate) async fn try_fast_paths(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    is_slash_command: bool,
) -> Result<bool, GatewayError> {
    let stop_text = incoming.text.trim();
    let stop_like_text = matches!(
        stop_text,
        "停止当前任务" | "停止当前任务。" | "停止任务" | "取消当前任务" | "取消任务"
    );

    if is_slash_command || stop_like_text {
        let command = if stop_like_text {
            GatewayCommandResult::StopAgent("⏹ Agent stopped.".to_string())
        } else {
            handle_command(&incoming.text)
        };
        if matches!(command, GatewayCommandResult::StopAgent(_)) {
            gw.apply_command_result(incoming, session_key, command)
                .await?;
            return Ok(true);
        }
    }

    if !is_slash_command {
        if let Some(dispatcher) = gw.extensions.clarify_dispatcher.read().await.as_ref() {
            if dispatcher
                .try_fulfill_for_session(
                    session_key,
                    &crate::tool_backends::extract_clarify_choice_token(&incoming.text),
                )
                .await
            {
                debug!(
                    session_key = %session_key,
                    platform = %incoming.platform,
                    chat_id = %incoming.chat_id,
                    text_chars = incoming.text.chars().count(),
                    "gateway clarify fast-path: inbound reply fulfilled active clarify wait"
                );
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Gets or creates the session, handles Discord backfill, emits session hooks,
/// and dispatches slash commands.
///
/// Returns `None` if the slash command was fully handled (caller should return
/// `Ok(())`).
pub(crate) async fn setup_session(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    is_slash_command: bool,
) -> Result<Option<Session>, GatewayError> {
    let is_dm = Some(incoming.is_dm);
    let existing_session = gw.session.session_manager.get_session(session_key).await;
    let session = gw
        .session
        .session_manager
        .get_or_create_session_with_dm(
            &incoming.platform,
            &incoming.chat_id,
            &incoming.user_id,
            is_dm,
        )
        .await;

    crate::telegram_topic::maybe_bind_new_topic_lane(incoming, session_key, &session.id);
    if let Some(bound_id) = crate::telegram_topic::bound_session_id(incoming) {
        gw.session
            .session_manager
            .apply_persisted_session_id(session_key, &bound_id)
            .await;
    }
    let session = gw
        .session
        .session_manager
        .get_session(session_key)
        .await
        .unwrap_or(session);

    tracing::trace!(
        platform = %incoming.platform,
        session_key = %session_key,
        session_started = existing_session.is_none(),
        "gateway session resolved"
    );

    #[cfg(feature = "discord")]
    if incoming.platform == "discord" {
        if let Some(adapter) = gw.extensions.discord_adapter.read().await.clone() {
            let _ = adapter
                .backfill_session_if_empty(
                    &gw.session.session_manager,
                    session_key,
                    &incoming.chat_id,
                )
                .await;
        }
    }

    let session_started = existing_session.is_none();
    let session_auto_reset = existing_session
        .as_ref()
        .map(|s| s.created_at != session.created_at)
        .unwrap_or(false);
    if session_started || session_auto_reset {
        gw.emit_hook_event(
            "session:start",
            serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "reason": if session_started { "new" } else { "auto_reset" }
            }),
        )
        .await;
    }

    if is_slash_command && gw.execute_slash_command(incoming, session_key).await? {
        return Ok(None);
    }

    Ok(Some(session))
}

/// Starts the typing keepalive and records the reaction adapter for finalization.
///
/// The returned `RouteTypingGuard` must be `.finish()`-ed after dispatch.
/// The returned adapter (if `Some`) is used by `finalize_reaction` to add the
/// completion emoji.
pub(crate) async fn begin_route_ux(
    gw: &Gateway,
    incoming: &IncomingMessage,
) -> (RouteTypingGuard, Option<Arc<dyn PlatformAdapter>>) {
    let typing_guard = if let Some(adapter) = gw.get_adapter(&incoming.platform).await {
        Gateway::spawn_route_typing(&incoming.platform, adapter, incoming.chat_id.clone())
    } else {
        RouteTypingGuard::none()
    };

    let reaction_adapter = if gw.should_apply_reaction_lifecycle(incoming).await {
        gw.get_adapter(&incoming.platform).await
    } else {
        None
    };

    if let (Some(adapter), Some(message_id)) = (&reaction_adapter, incoming.message_id.as_deref()) {
        if let Err(err) = adapter
            .add_reaction(&incoming.chat_id, message_id, "eyes")
            .await
        {
            debug!(
                platform = incoming.platform,
                chat_id = incoming.chat_id,
                message_id = message_id,
                "Failed to add start reaction: {}",
                err
            );
        }
    }

    (typing_guard, reaction_adapter)
}

/// Prepares and appends the inbound user message, snapshots the conversation,
/// and logs route-start info.
///
/// Returns the conversation snapshot and the preparation elapsed time in ms.
pub(crate) async fn prepare_user_turn(
    gw: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    route_id: &str,
    route_start: Instant,
) -> Result<(Arc<Vec<Message>>, u64), GatewayError> {
    let user_message = gw.prepare_inbound_user_message(incoming, session_key).await;
    let input_chars = user_message
        .content
        .as_deref()
        .unwrap_or("")
        .chars()
        .count();

    let messages = gw
        .session
        .session_manager
        .append_and_snapshot(session_key, user_message)
        .await;
    gw.bump_input_usage(session_key, input_chars).await;

    let session_transcript_chars: usize = messages
        .iter()
        .map(|m| m.content.as_deref().map(|c| c.chars().count()).unwrap_or(0))
        .sum();
    if incoming.platform.eq_ignore_ascii_case("discord") {
        info!(
            platform = %incoming.platform,
            session_key = %session_key,
            chat_id = %incoming.chat_id,
            user_id = %incoming.user_id,
            is_dm = incoming.is_dm,
            message_count = messages.len(),
            session_transcript_chars = session_transcript_chars,
            inbound_text_chars = input_chars,
            has_media = !incoming.media_urls.is_empty(),
            "Discord session context snapshot before agent"
        );
    }

    let (text_chars, text_preview, text_fp) = inbound_text_log_fields(&incoming.text);
    info!(
        route_id = %route_id,
        platform = %incoming.platform,
        chat_id = %incoming.chat_id,
        user_id = %incoming.user_id,
        session_key = %session_key,
        is_dm = incoming.is_dm,
        has_media = !incoming.media_urls.is_empty(),
        text_chars = text_chars,
        text_preview = %text_preview,
        text_fp = %text_fp,
        message_count = messages.len(),
        "gateway route start"
    );

    let prep_ms = route_start.elapsed().as_millis() as u64;
    Ok((messages, prep_ms))
}

/// Dispatches the prepared messages to the agent handler (streaming or one-shot),
/// wraps the future in an `AbortHandle` for stop-command interruption, and logs
/// the final timing.
///
/// Requires `&SessionGuard` to enforce that `active_routes` is only mutated
/// while the session serial is held (see session_layer module doc).
pub(crate) async fn dispatch_agent_route(
    gw: &Gateway,
    incoming: &IncomingMessage,
    guard: &SessionGuard,
    route_id: &str,
    messages: Arc<Vec<Message>>,
    route_start: Instant,
) -> Result<(), GatewayError> {
    let session_key = &guard.key;

    let supports_streaming = gw.config.streaming_enabled
        // WeCom native stream flush: iLink API does not support message edits.
        && !incoming.platform.eq_ignore_ascii_case("weixin")
        // WhatsApp (wa-rs) "..." placeholder + edit is unreliable; use one-shot.
        && !incoming.platform.eq_ignore_ascii_case("whatsapp");

    if incoming.platform.eq_ignore_ascii_case("wecom") {
        info!(
            route_id = %route_id,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            streaming_enabled = gw.config.streaming_enabled,
            supports_streaming,
            message_id = ?incoming.message_id,
            "wecom route: streaming vs one-shot decision"
        );
    }

    gw.begin_turn_outbound_tracking(session_key, &incoming.platform, &incoming.chat_id);

    let process_start = Instant::now();
    let route_future = async {
        if supports_streaming {
            gw.route_streaming(incoming, messages, session_key, route_id)
                .await
        } else {
            gw.route_non_streaming(incoming, messages, session_key, route_id)
                .await
        }
    };

    // Guard-gated insert/remove enforces that active_routes is only mutated
    // while session_serial is held.
    let (abort_handle, abort_reg) = AbortHandle::new_pair();
    gw.session.register_route(guard, abort_handle).await;

    let routed = Abortable::new(route_future, abort_reg).await;

    gw.session.unregister_route(guard).await;
    gw.clear_turn_outbound_tracking(session_key);

    let processing_ms = process_start.elapsed().as_millis() as u64;
    let result = match routed {
        Ok(result) => result,
        Err(_) => {
            info!(
                route_id = %route_id,
                session_key = %session_key,
                "gateway route aborted by stop command"
            );
            Ok(())
        }
    };

    info!(
        route_id = %route_id,
        platform = %incoming.platform,
        chat_id = %incoming.chat_id,
        session_key = %session_key,
        processing_ms = processing_ms,
        elapsed_ms = route_start.elapsed().as_millis() as u64,
        success = result.is_ok(),
        "gateway route finished"
    );

    result
}

/// Removes the "eyes" reaction and adds the completion emoji (✅ or ❌).
pub(crate) async fn finalize_reaction(
    reaction_adapter: Option<Arc<dyn PlatformAdapter>>,
    incoming: &IncomingMessage,
    result: &Result<(), GatewayError>,
) {
    let (Some(adapter), Some(message_id)) = (reaction_adapter, incoming.message_id.as_deref())
    else {
        return;
    };

    if let Err(err) = adapter
        .remove_reaction(&incoming.chat_id, message_id, "eyes")
        .await
    {
        debug!(
            platform = incoming.platform,
            chat_id = incoming.chat_id,
            message_id = message_id,
            "Failed to remove start reaction: {}",
            err
        );
    }

    let emoji = if result.is_ok() {
        "white_check_mark"
    } else {
        "x"
    };
    if let Err(err) = adapter
        .add_reaction(&incoming.chat_id, message_id, emoji)
        .await
    {
        debug!(
            platform = incoming.platform,
            chat_id = incoming.chat_id,
            message_id = message_id,
            "Failed to add completion reaction: {}",
            err
        );
    }
}
