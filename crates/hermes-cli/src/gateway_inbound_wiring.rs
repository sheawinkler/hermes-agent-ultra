//! Wire auxiliary vision + inbound preparer + voice/STT into gateway and tool registry.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use hermes_agent::{
    auxiliary_config_from_gateway, build_auxiliary_client,
    register_agent_builtin_tools_with_voice, AgentInboundPreparer, AuxiliaryBuildParams,
};
use hermes_config::GatewayConfig;
use hermes_core::{SkillProvider, TerminalBackend};
use hermes_gateway::voice::VoiceManager;
use hermes_gateway::voice_config::voice_config_from_app;
use hermes_gateway::Gateway;
use hermes_tools::{ToolRegistry, VoiceMediaToolConfig};

/// Parse `provider:model` from config (e.g. `custom:flowy/DeepSeek-V4-Flash`).
fn split_configured_model(model: &str) -> (Option<String>, Option<String>) {
    let trimmed = model.trim();
    if let Some((provider, rest)) = trimmed.split_once(':') {
        let provider = provider.trim();
        let rest = rest.trim();
        if !provider.is_empty() && !rest.is_empty() {
            return (Some(provider.to_string()), Some(rest.to_string()));
        }
    }
    (None, Some(trimmed.to_string()))
}

/// Build auxiliary client, vision tool backend, gateway inbound preparer, and voice runtime from config.
pub async fn wire_gateway_inbound_vision(
    gateway: &Arc<Gateway>,
    tool_registry: &Arc<ToolRegistry>,
    config: &GatewayConfig,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
) {
    let configured = config
        .model
        .as_deref()
        .unwrap_or("gpt-4o")
        .to_string();
    let (primary_provider, primary_model) = split_configured_model(&configured);

    let (auxiliary, _summary) = build_auxiliary_client(AuxiliaryBuildParams {
        config: auxiliary_config_from_gateway(config),
        primary_provider: primary_provider.clone(),
        primary_model: primary_model.clone(),
        llm_providers: config.llm_providers.clone(),
    });

    let auxiliary = Arc::new(auxiliary);
    let tts_cfg: Option<hermes_config::voice::TtsConfig> = if config.tts.is_null() {
        None
    } else {
        serde_json::from_value(config.tts.clone()).ok()
    };
    let stt_cfg: Option<hermes_config::voice::SttConfig> = if config.stt.is_null() {
        None
    } else {
        serde_json::from_value(config.stt.clone()).ok()
    };
    let voice_tools = VoiceMediaToolConfig {
        tts: tts_cfg.clone(),
        stt: stt_cfg.clone(),
    };
    register_agent_builtin_tools_with_voice(
        tool_registry,
        terminal_backend,
        skill_provider,
        Some(auxiliary.clone()),
        Some(voice_tools),
    );

    let preparer = Arc::new(AgentInboundPreparer::new(auxiliary));
    gateway.set_inbound_preparer(preparer).await;

    let (voice_cfg, stt_enabled) = voice_config_from_app(tts_cfg.as_ref(), stt_cfg.as_ref());
    let stt_config = hermes_config::voice::SttConfig::default();
    let manager = Arc::new(VoiceManager::with_stt_config(voice_cfg, stt_config));
    gateway.set_voice_runtime(manager, stt_enabled).await;
}

/// Truncate text for gateway status / thinking previews (Unicode-safe).
pub fn truncate_gateway_preview(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(max_chars).collect();
    format!("{prefix}…")
}

/// Format reasoning / chain-of-thought for a standalone chat message.
pub fn format_gateway_thinking_message(text: &str) -> String {
    let preview = truncate_gateway_preview(text, 2000);
    if preview.is_empty() {
        return String::new();
    }
    format!("🧠 {preview}")
}

/// Suppress noisy lifecycle lines that duplicate streamed thinking output.
///
/// Compression / context-pressure status is still emitted to `agent:status` hooks
/// but must not become separate WeCom/Telegram chat messages (each costs a round trip).
pub fn gateway_status_message_visible(event_type: &str, message: &str) -> bool {
    if message.trim().is_empty() {
        return false;
    }
    if event_type == "tool_progress" || event_type == "tool_failure" {
        return true;
    }
    if event_type != "lifecycle" {
        return true;
    }
    let suppressed = [
        "Reasoning-only response",
        "Preflight compression",
        "Context pressure",
        "Context still at",
        "triggering compression",
        "ContextLattice",
        "Compaction governance",
        "Assistant response incomplete",
        "Continuation retries exhausted",
        "requesting continuation",
        "Detected intermediate ack",
        "Truncated tool arguments",
        "Parsed textual tool-call markup",
        "Starting conversation",
    ];
    !suppressed.iter().any(|needle| message.contains(needle))
}

/// Status + hook emitter for gateway agent runs (compression, context pressure, etc.).
pub fn make_gateway_status_callback(
    gateway: Arc<Gateway>,
    platform: String,
    chat_id: String,
    user_id: String,
    session_id: String,
) -> Arc<dyn Fn(&str, &str) + Send + Sync> {
    let progress_message_id: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
    Arc::new(move |event_type: &str, message: &str| {
        if !gateway_status_message_visible(event_type, message) {
            return;
        }
        let outbound = if event_type == "thinking" {
            format_gateway_thinking_message(message)
        } else {
            message.to_string()
        };
        if outbound.trim().is_empty() {
            return;
        }
        let gw = gateway.clone();
        let platform_msg = platform.clone();
        let chat_id_msg = chat_id.clone();
        let msg = outbound;
        let progress_mode = hermes_gateway::display_config::resolve_display_setting(
            None,
            &platform_msg,
            "tool_progress",
            None,
        );
        let reuse_progress = event_type == "tool_progress" && progress_mode.as_deref() == Some("new");
        let progress_id = progress_message_id.clone();
        tokio::spawn(async move {
            if reuse_progress {
                let existing = progress_id.lock().unwrap().clone();
                if let Some(mid) = existing {
                    if gw
                        .edit_message(&platform_msg, &chat_id_msg, &mid, &msg)
                        .await
                        .is_ok()
                    {
                        return;
                    }
                }
                if let Ok(Some(mid)) = gw
                    .send_message_with_id(&platform_msg, &chat_id_msg, &msg, None)
                    .await
                {
                    *progress_id.lock().unwrap() = Some(mid);
                    return;
                }
            }
            let _ = gw
                .send_message(&platform_msg, &chat_id_msg, &msg, None)
                .await;
        });
        let gw_hook = gateway.clone();
        let platform_hook = platform.clone();
        let user_id = user_id.clone();
        let session_id = session_id.clone();
        let event_type = event_type.to_string();
        let message = message.to_string();
        tokio::spawn(async move {
            gw_hook
                .emit_hook_event(
                    "agent:status",
                    serde_json::json!({
                        "platform": platform_hook,
                        "user_id": user_id,
                        "session_id": session_id,
                        "event_type": event_type,
                        "message": message
                    }),
                )
                .await;
        });
    })
}

/// Gateway thinking handler — log only; do not send per-delta WeCom messages.
///
/// Reasoning must not be multiplexed into the answer stream (pollutes the reply) and
/// must not be sent as separate chat messages per token (very slow on WeCom).
/// Native streaming already shows a「思考中…」placeholder until content arrives.
pub fn make_gateway_on_thinking_callback(
    _gateway: Arc<Gateway>,
    _platform: String,
    _chat_id: String,
    _stream_emit: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> Box<dyn Fn(&str) + Send + Sync> {
    struct ThinkingLogState {
        started_at: Instant,
        last_flush: Instant,
        delta_count: u64,
        total_chars: usize,
    }
    let state = Arc::new(StdMutex::new(ThinkingLogState {
        started_at: Instant::now(),
        last_flush: Instant::now(),
        delta_count: 0,
        total_chars: 0,
    }));
    Box::new(move |thinking: &str| {
        if thinking.trim().is_empty() {
            return;
        }
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.delta_count = guard.delta_count.saturating_add(1);
        guard.total_chars = guard.total_chars.saturating_add(thinking.chars().count());
        let now = Instant::now();
        if now.duration_since(guard.last_flush) >= Duration::from_millis(1000) || guard.delta_count >= 32 {
            tracing::debug!(
                thinking_delta_count = guard.delta_count,
                thinking_total_chars = guard.total_chars,
                thinking_window_ms = now.duration_since(guard.last_flush).as_millis() as u64,
                thinking_elapsed_ms = now.duration_since(guard.started_at).as_millis() as u64,
                "gateway thinking deltas aggregated (not sent to chat)"
            );
            guard.delta_count = 0;
            guard.total_chars = 0;
            guard.last_flush = now;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_status_shows_tool_failure() {
        assert!(gateway_status_message_visible(
            "tool_failure",
            "处理中：该网页拒绝自动抓取，正在尝试浏览器打开…"
        ));
    }

    #[test]
    fn gateway_status_shows_tool_progress() {
        assert!(gateway_status_message_visible(
            "tool_progress",
            "处理中：正在检索网络数据（第 1 步，工具 web_search）…"
        ));
    }

    #[test]
    fn gateway_status_hides_agent_continuation_lifecycle() {
        assert!(!gateway_status_message_visible(
            "lifecycle",
            "Assistant response incomplete (Some(\"tool_calls\")) - requesting continuation (1/3)"
        ));
        assert!(!gateway_status_message_visible(
            "lifecycle",
            "Continuation retries exhausted (3) - finalizing with best effort output"
        ));
    }

    #[test]
    fn gateway_status_hides_compression_lifecycle() {
        assert!(!gateway_status_message_visible(
            "lifecycle",
            "Preflight compression check: 837% context usage"
        ));
        assert!(!gateway_status_message_visible(
            "lifecycle",
            "Context pressure at 760%, triggering compression"
        ));
        assert!(gateway_status_message_visible(
            "lifecycle",
            "会话上下文仍超过窗口容量（约 90%）。请发送 /new"
        ));
    }
}
