impl AgentLoop {
    // -- Plugin hook helpers ------------------------------------------------

    fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        if let Some(ref pm) = self.plugin_manager {
            if let Ok(pm) = pm.lock() {
                return pm.invoke_hook(hook, ctx_val);
            }
        }
        Vec::new()
    }

    fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        for r in results {
            if let HookResult::InjectContext(text) = r {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(spill_path) = self.spill_hook_context_if_oversized(trimmed) {
                    let preview = truncate_hook_preview(trimmed, 720);
                    let note = format!(
                        "Hook context was oversized and spilled to disk.\nspill_path={}\npreview:\n{}",
                        spill_path.display(),
                        preview
                    );
                    ctx.add_message(Message::system(note));
                    continue;
                }
                ctx.add_message(Message::system(trimmed.to_string()));
            }
        }
    }

    fn apply_hook_output_transforms(&self, results: &[HookResult], content: &mut Option<String>) {
        let mut current = content.clone().unwrap_or_default();
        let mut changed = false;
        for r in results {
            if let HookResult::TransformLlmOutput(next) = r {
                current = next.clone();
                changed = true;
            }
        }
        if changed {
            *content = Some(current);
        }
    }

    fn apply_tool_request_middleware_to_calls(&self, tool_calls: &mut [ToolCall], turn: u32) {
        let Some(ref pm) = self.plugin_manager else {
            return;
        };
        let Ok(pm) = pm.lock() else {
            tracing::warn!("Plugin manager lock poisoned while applying tool request middleware");
            return;
        };
        for tc in tool_calls {
            let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) else {
                continue;
            };
            let result = pm.apply_tool_request_middleware(&tc.function.name, args, turn);
            if !result.changed {
                continue;
            }
            match serde_json::to_string(&result.args) {
                Ok(serialized) => tc.function.arguments = serialized,
                Err(err) => tracing::warn!(
                    tool = tc.function.name,
                    error = %err,
                    "Plugin tool request middleware returned non-serializable arguments"
                ),
            }
        }
    }

    fn hook_context_spill_threshold_chars(&self) -> usize {
        std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v >= 1024)
            .unwrap_or(12_000)
    }

    fn hook_context_spill_dir(&self) -> PathBuf {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("hooks").join("spills")
    }

    fn spill_hook_context_if_oversized(&self, text: &str) -> Option<PathBuf> {
        if text.len() < self.hook_context_spill_threshold_chars() {
            return None;
        }
        let dir = self.hook_context_spill_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        let digest = hex::encode(hasher.finalize());
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let path = dir.join(format!("hook_context_{}_{}.txt", stamp, &digest[..16]));
        if std::fs::write(&path, text).is_ok() {
            Some(path)
        } else {
            None
        }
    }

    // -- Memory helpers ----------------------------------------------------

    fn memory_prefetch(&self, query: &str, session_id: &str) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.prefetch_all(query, session_id);
            }
        }
        String::new()
    }

    fn memory_sync(&self, user: &str, assistant: &str, session_id: &str, messages: &[Message]) {
        if self.config.skip_memory {
            return;
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                mm.sync_all_with_messages(user, assistant, session_id, messages);
                if !user.trim().is_empty() {
                    mm.queue_prefetch_all(user, session_id);
                }
            }
        }
    }

    fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
        if tc.function.name != "memory" {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_lowercase();
        if action != "add" && action != "replace" && action != "remove" {
            return None;
        }
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("memory")
            .to_string();
        let content = if action == "remove" {
            args.get("old_text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        } else {
            args.get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        Some((action, target, content))
    }

    fn memory_tool_result_succeeded(content: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(content) else {
            return false;
        };
        let Some(object) = value.as_object() else {
            return false;
        };
        object.get("success").and_then(Value::as_bool) == Some(true)
            && object.get("staged").and_then(Value::as_bool) != Some(true)
    }

    fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mut mm) = mm.lock() else {
            return;
        };
        for result in results {
            if result.is_error {
                continue;
            }
            if !Self::memory_tool_result_succeeded(&result.content) {
                continue;
            }
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((action, target, content)) = Self::memory_write_event_from_tool_call(tc)
            else {
                continue;
            };
            mm.on_memory_write(&action, &target, &content);
        }
    }

    fn delegation_event_from_tool_result(
        tc: &ToolCall,
        result: &ToolResult,
    ) -> Option<(String, String)> {
        if tc.function.name != "delegate_task" || result.is_error {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        let sub_agent_id = serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|v| {
                v.get("sub_agent_id")
                    .and_then(|id| id.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
            .unwrap_or_default();

        Some((task, sub_agent_id))
    }

    fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        for result in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((task, sub_agent_id)) = Self::delegation_event_from_tool_result(tc, result)
            else {
                continue;
            };
            mm.on_delegation(&task, &sub_agent_id);
        }
    }

    fn memory_on_turn_start(&self, turn: u32, message: &str) {
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mut mm) = mm.lock() {
                mm.on_turn_start(turn, message);
            }
        }
    }

    fn memory_system_prompt(&self) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.build_system_prompt();
            }
        }
        String::new()
    }

    fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        if self.config.skip_memory {
            return None;
        }
        let Some(ref mm) = self.memory_manager else {
            return None;
        };
        let Ok(mm) = mm.lock() else {
            return None;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let note = mm.on_pre_compress(&as_values);
        if note.trim().is_empty() {
            None
        } else {
            Some(note)
        }
    }

    fn memory_on_session_end(&self, messages: &[Message]) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref mm) = self.memory_manager else {
            return;
        };
        let Ok(mm) = mm.lock() else {
            return;
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        mm.on_session_end(&as_values);
    }

    fn code_index_repo_map_block(&self) -> Option<String> {
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        let rendered = idx.render_repo_map(
            Some(self.config.code_index_max_files),
            Some(self.config.code_index_max_symbols),
        );
        if rendered.trim().is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    fn lsp_context_note(&self, tool_calls: &[ToolCall], results: &[ToolResult]) -> Option<String> {
        if !self.lsp_context.enabled {
            return None;
        }
        let Some(ref idx) = self.code_index else {
            return None;
        };
        let Ok(mut idx) = idx.lock() else {
            return None;
        };
        build_lsp_context_note(tool_calls, results, &mut idx, &self.lsp_context)
    }

    fn should_inject_tool_enforcement(&self, model: &str) -> bool {
        should_inject_tool_enforcement_for_model(model)
    }

    fn platform_hint_text(&self) -> Option<&'static str> {
        let platform_key = self
            .config
            .platform
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        match platform_key.as_str() {
            "cli" => Some("You are a CLI AI Agent. Prefer concise plain text output suitable for terminals."),
            "telegram" | "discord" | "slack" => {
                Some("You are responding on a chat platform. Keep responses concise and avoid heavy formatting.")
            }
            "email" => Some("You are responding over email. Use clear structure and complete sentences."),
            "sms" => Some("You are responding over SMS. Keep responses short and high-signal."),
            _ => None,
        }
    }

    fn effective_provider_for_prompt(&self, model: &str) -> Option<String> {
        if let Some(ref p) = self.config.provider {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        model
            .split_once(':')
            .map(|(provider, _)| provider.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn runtime_skills_tier() -> &'static str {
        match std::env::var("HERMES_SKILLS_EXECUTION_TIER")
            .ok()
            .unwrap_or_else(|| "balanced".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "trusted" => "trusted",
            "open" | "permissive" => "open",
            _ => "balanced",
        }
    }

    fn runtime_skills_tier_bypass_enabled() -> bool {
        std::env::var("HERMES_SKILLS_TIER_BYPASS")
            .ok()
            .is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
    }

    fn skill_trust_score(cmd: &str, name: &str, description: &str) -> i32 {
        let corpus = format!(
            "{} {} {}",
            cmd.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
            description.to_ascii_lowercase()
        );
        let mut score = 70i32;
        let high_risk_terms = [
            "trade",
            "money",
            "wallet",
            "deploy",
            "delete",
            "shell",
            "execute",
            "terminal",
            "browser automation",
            "computer use",
            "send email",
            "gmail",
            "calendar",
        ];
        for term in high_risk_terms {
            if corpus.contains(term) {
                score -= 12;
            }
        }
        let medium_risk_terms = ["write", "modify", "edit", "publish", "install", "webhook"];
        for term in medium_risk_terms {
            if corpus.contains(term) {
                score -= 6;
            }
        }
        let trusted_terms = ["search", "read", "summarize", "analyze", "query", "list"];
        for term in trusted_terms {
            if corpus.contains(term) {
                score += 4;
            }
        }
        score.clamp(0, 100)
    }

    fn skill_allowed_for_tier(tier: &str, score: i32) -> bool {
        match tier {
            "trusted" => score >= 62,
            "balanced" => score >= 34,
            _ => true,
        }
    }

    fn skills_system_prompt(
        &self,
        tool_names: &HashSet<&str>,
        hidden_categories: &[&str],
    ) -> Option<String> {
        let has_skills_tools = ["skills_list", "skill_view", "skill_manage"]
            .iter()
            .any(|t| tool_names.contains(*t));
        if !has_skills_tools {
            return None;
        }
        let skills_dir = self
            .config
            .hermes_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HERMES_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
            })
            .map(|home| home.join("skills"))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".hermes")
                    .join("skills")
            });
        let mut orch = SkillOrchestrator::new(skills_dir.clone());
        orch.set_enabled_disabled(&self.config.enabled_skills, &self.config.disabled_skills);
        let commands = orch.scan_skill_commands();
        if commands.is_empty() {
            return Some(
                "## Skills (mandatory)\nSkills tools are enabled. Use `skills_list` to discover available skills and `skill_view` before applying one."
                    .to_string(),
            );
        }
        let tier = Self::runtime_skills_tier();
        let bypass = Self::runtime_skills_tier_bypass_enabled();
        let mut rows: Vec<_> = commands
            .iter()
            .filter(|(cmd, info)| {
                if let Some(category) = info
                    .skill_dir
                    .strip_prefix(&skills_dir)
                    .ok()
                    .and_then(|relative| relative.components().next())
                    .and_then(|component| component.as_os_str().to_str())
                {
                    if hidden_categories
                        .iter()
                        .any(|hidden| category.eq_ignore_ascii_case(hidden))
                    {
                        return false;
                    }
                }
                if bypass || tier == "open" {
                    return true;
                }
                let score = Self::skill_trust_score(cmd, &info.name, &info.description);
                Self::skill_allowed_for_tier(tier, score)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let filtered = commands.len().saturating_sub(rows.len());
        if rows.is_empty() {
            return Some(format!(
                "## Skills (mandatory)\nSkills tools are enabled but current skills tier '{}' filtered all candidates. Use `/ops skills-tier balanced` or `/ops skills-tier open` for broader access.",
                tier
            ));
        }
        let mut body = String::from(
            "## Skills (mandatory)\nBefore replying, check whether an existing skill applies. If yes, inspect it with `skill_view` and follow it.\n<available_skills>\n",
        );
        body.push_str(&format!(
            "<skills_tier mode=\"{}\" bypass=\"{}\" filtered=\"{}\" />\n",
            tier,
            if bypass { "on" } else { "off" },
            filtered
        ));
        if !hidden_categories.is_empty() {
            body.push_str(&format!(
                "<skills_prompt_pruned hidden_categories=\"{}\" disclosure=\"full catalog remains available through skills_list and skill_view\" />\n",
                hidden_categories.join(",")
            ));
        }
        for (cmd, info) in rows.into_iter().take(80) {
            body.push_str(&format!(
                "- {}: {} ({})\n",
                cmd,
                info.name,
                info.description.trim()
            ));
        }
        body.push_str("</available_skills>");
        Some(body)
    }

    fn google_workspace_paths(&self) -> (PathBuf, PathBuf, PathBuf) {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HERMES_AGENT_ULTRA_HOME")
                    .ok()
                    .map(PathBuf::from)
            })
            .or_else(|| dirs::home_dir().map(|home| home.join(".hermes-agent-ultra")))
            .unwrap_or_else(|| PathBuf::from(".hermes-agent-ultra"));
        let skill_dir = hermes_home
            .join("skills")
            .join("productivity")
            .join("google-workspace");
        let setup = skill_dir.join("scripts").join("setup.py");
        let api = skill_dir.join("scripts").join("google_api.py");
        (hermes_home, setup, api)
    }

    fn google_workspace_system_hint(
        &self,
        messages: &[Message],
        tool_schemas: &[ToolSchema],
    ) -> Option<String> {
        if !detect_google_workspace_intent(messages) {
            return None;
        }
        let has_skills = tool_schemas
            .iter()
            .any(|tool| matches!(tool.name.as_str(), "skills_list" | "skill_view"));
        let has_terminal = tool_schemas.iter().any(|tool| tool.name == "terminal");
        let (home, setup, api) = self.google_workspace_paths();
        Some(format!(
            "[SYSTEM] Google Workspace/Gmail execution contract active. \
             If Gmail or Google Workspace is requested, first use `skills_list`, then `skill_view` for `google-workspace` when available. \
             Do not invent credential paths such as `~/.config/hermes/credentials.toml`; Hermes Google Workspace stores OAuth state under `{home}` (`google_token.json`, `google_client_secret.json`). \
             If terminal is available, use direct commands, not `bash -lc`/`sh -c`/`zsh -c`: \
             `env HERMES_HOME={home} python3.12 {setup} --check` (fallback `env HERMES_HOME={home} python3 {setup} --check` if python3.12 is unavailable), then `env HERMES_HOME={home} python3.12 {api} gmail search \"newer_than:30d\" --max 10` only if setup is authenticated. \
             If access is blocked, final answer must include `GOOGLE_WORKSPACE_USED: no`, the exact setup/API command attempted, and the exact tool error. \
             Tool availability: skills_tools={has_skills}, terminal={has_terminal}.",
            home = home.display(),
            setup = setup.display(),
            api = api.display(),
            has_skills = has_skills,
            has_terminal = has_terminal
        ))
    }

    fn google_workspace_retry_prompt(&self) -> String {
        let (home, setup, api) = self.google_workspace_paths();
        format!(
            "[SYSTEM] Google Workspace evidence contract failed. \
             Re-run the task using the real Hermes Google Workspace skill paths. \
             Required now: \
             1) call `skill_view` for `google-workspace` if not already done; \
             2) call terminal with a direct command, not `bash -lc`: `env HERMES_HOME={home} python3.12 {setup} --check`; \
             3) if python3.12 is unavailable, call `env HERMES_HOME={home} python3 {setup} --check`; \
             4) if setup reports authenticated, call `env HERMES_HOME={home} python3.12 {api} gmail search \"newer_than:30d\" --max 10` and then read relevant messages with `gmail get`; \
             5) if setup reports NOT_AUTHENTICATED/no token, stop and final-answer that blocker; do not search broad filesystem locations and do not invent email results; \
             6) if blocked, final answer must include `GOOGLE_WORKSPACE_USED: no`, `cmd=<exact command>`, and the exact error. \
             Use `{home}` for Hermes credential/token paths; do not use `~/.config/hermes/credentials.toml`.",
            home = home.display(),
            setup = setup.display(),
            api = api.display()
        )
    }

    fn context_files_prompt(&self) -> Option<String> {
        if self.config.skip_context_files {
            return None;
        }
        let cwd = std::env::var("TERMINAL_CWD")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });

        let mut sections = Vec::new();
        if let Some(workspace) = load_workspace_context(&cwd) {
            sections.push(format!("## Workspace Context\n{}", workspace));
        }

        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HERMES_HOME")
                    .ok()
                    .map(std::path::PathBuf::from)
            })
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| std::path::PathBuf::from(".hermes"));

        let personal_ctx = load_hermes_context_files(&hermes_home);
        if !personal_ctx.trim().is_empty() {
            sections.push(format!("## Personal Context\n{}", personal_ctx));
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

}
