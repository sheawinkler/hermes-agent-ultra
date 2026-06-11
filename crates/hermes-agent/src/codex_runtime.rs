//! Codex app-server runtime — parity with `agent/codex_runtime.py`.

use std::sync::Arc;
use std::time::Duration;

use hermes_core::Message;

use crate::agent_loop::{AgentLoop, LoopExit};
use crate::context::ContextManager;
use crate::transports::codex_app_server::check_codex_binary;
use crate::transports::codex_app_server_session::CodexAppServerSession;

impl AgentLoop {
    /// True when the active runtime is the codex app-server path.
    pub(crate) fn api_mode_is_codex_app_server(&self) -> bool {
        use crate::smart_model_routing::ApiMode;
        matches!(
            crate::route_learning::primary_runtime_snapshot(self).api_mode,
            ApiMode::CodexAppServer
        )
    }

    fn session_cwd(&self) -> String {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    }

    /// Drive one user turn through codex app-server (Python `run_codex_app_server_turn`).
    pub(crate) async fn run_codex_app_server_turn(
        &self,
        user_message: &str,
        mut messages: Vec<Message>,
        should_review_memory: bool,
        session_started_hooks_fired: bool,
    ) -> hermes_core::AgentResult {
        let codex_bin = std::env::var("HERMES_CODEX_BIN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "codex".to_string());

        let (ok, version_msg) = check_codex_binary(&codex_bin);
        if !ok {
            return self.codex_turn_failure(
                messages,
                &version_msg,
                session_started_hooks_fired,
                "codex_app_server_binary_missing",
            );
        }
        tracing::info!(codex_version = %version_msg, "codex app-server binary ok");

        // Extract the codex session from shared state for use in spawn_blocking.
        // We wrap it in a standalone Arc<Mutex<>> so the blocking closure does not
        // hold the global AgentSharedState lock during the session's run_turn call.
        let codex_session_arc: Arc<std::sync::Mutex<Option<CodexAppServerSession>>> =
            Arc::new(std::sync::Mutex::new(
                self.state
                    .lock()
                    .map(|mut state| state.codex_app_server_session.take())
                    .unwrap_or(None),
            ));
        let turn_result = tokio::task::spawn_blocking({
            let slot = codex_session_arc.clone();
            let interrupt = Arc::new(self.interrupt.clone());
            let cwd = self.session_cwd();
            let codex_home = self.config().hermes_home.clone();
            let user = user_message.to_string();
            let approval_callback = self.callbacks.codex_approval_callback.clone();
            move || -> Result<crate::transports::codex_app_server_session::TurnResult, String> {
                let mut guard = slot
                    .lock()
                    .map_err(|_| "codex session lock poisoned".to_string())?;
                if guard.is_none() {
                    *guard = Some(CodexAppServerSession::with_approval_callback(
                        cwd,
                        interrupt,
                        Some(codex_bin),
                        codex_home,
                        approval_callback,
                    ));
                }
                let session = guard
                    .as_mut()
                    .ok_or_else(|| "codex session missing".to_string())?;
                Ok(session.run_turn(
                    &user,
                    Duration::from_secs(600),
                    Duration::from_millis(250),
                    Duration::from_secs(90),
                ))
            }
        })
        .await;

        let turn = match turn_result {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                return self.codex_turn_failure(
                    messages,
                    &e,
                    session_started_hooks_fired,
                    "codex_app_server_session_error",
                );
            }
            Err(e) => {
                return self.codex_turn_failure(
                    messages,
                    &e.to_string(),
                    session_started_hooks_fired,
                    "codex_app_server_session_error",
                );
            }
        };

        if turn.should_retire {
            if let Ok(mut guard) = codex_session_arc.lock() {
                if let Some(mut session) = guard.take() {
                    session.close();
                }
            }
        }

        if !turn.projected_messages.is_empty() {
            messages.extend(turn.projected_messages);
        }

        if let Some(ref err) = turn.error {
            if turn.final_text.trim().is_empty() {
                messages.push(Message::assistant(format!(
                    "Codex app-server turn failed: {err} \
                     Fall back to default runtime with `/codex-runtime auto`."
                )));
            }
        }

        // Store the codex session back into shared state.
        if let Ok(mut state) = self.state.lock() {
            state.codex_app_server_session =
                codex_session_arc.lock().ok().and_then(|mut g| g.take());
        }

        if !turn.interrupted && turn.error.is_none() {
            if let Ok(mut state) = self.state.lock() {
                state.evolution_counters.iters_since_skill = state
                    .evolution_counters
                    .iters_since_skill
                    .saturating_add(turn.tool_iterations);
            }
        }

        let _ = should_review_memory;

        hermes_telemetry::record_codex_turn(turn.tool_iterations);

        let mut ctx =
            ContextManager::for_model(crate::runtime_provider::active_model(self).as_str());
        for msg in &messages {
            ctx.add_message(msg.clone());
        }
        crate::hooks::turn_end_plugin_hooks(
            self,
            ctx.get_messages(),
            false,
            turn.interrupted,
            0,
            session_started_hooks_fired,
        );

        let completed = !turn.interrupted && turn.error.is_none();
        let partial = turn.interrupted || turn.error.is_some();
        let exit_reason = if turn.error.is_some() {
            "codex_app_server_error"
        } else if turn.interrupted {
            "codex_app_server_interrupted"
        } else {
            "codex_app_server_turn"
        };

        self.seal_loop_result(
            &ctx,
            None,
            None,
            LoopExit::base(
                exit_reason,
                if completed || partial { 1 } else { 0 },
                false,
                partial,
                completed,
                turn.interrupted,
            ),
            0,
            Vec::new(),
            None,
            0.0,
            session_started_hooks_fired,
        )
    }

    fn codex_turn_failure(
        &self,
        mut messages: Vec<Message>,
        err: &str,
        session_started_hooks_fired: bool,
        reason: &'static str,
    ) -> hermes_core::AgentResult {
        tracing::warn!(error = %err, "codex app-server turn failed");
        messages.push(Message::assistant(format!(
            "Codex app-server turn failed: {err} \
             Fall back to default runtime with `/codex-runtime auto`."
        )));
        let mut ctx =
            ContextManager::for_model(crate::runtime_provider::active_model(self).as_str());
        for msg in messages {
            ctx.add_message(msg);
        }
        self.seal_loop_result(
            &ctx,
            None,
            None,
            LoopExit::base(reason, 0, false, true, false, false),
            0,
            Vec::new(),
            None,
            0.0,
            session_started_hooks_fired,
        )
    }
}
