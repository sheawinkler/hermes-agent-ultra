fn build_live_cron_scheduler(cli: &Cli, data_dir: &Path) -> Result<CronScheduler, AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let current_model = config
        .model
        .clone()
        .unwrap_or_else(|| "gpt-5.5".to_string());
    let current_model = select_startup_model_with_fallback_and_auth_resolver(
        &config,
        &current_model,
        Some(&provider_oauth_token_from_auth_state),
    )
    .selected_model;
    let provider = build_provider(&config, &current_model);

    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&config);
    let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);

    let runner = Arc::new(CronRunner::new(
        provider,
        Arc::new(bridge_tool_registry(&tool_registry)),
    ));
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir.to_path_buf()));
    Ok(CronScheduler::new(persistence, runner))
}

fn parse_deliver_config(
    raw: &str,
    deliver_chat_id: Option<&str>,
) -> Option<hermes_cron::DeliverConfig> {
    let trimmed = raw.trim();
    let (target_raw, inline_chat_id) = trimmed
        .split_once(':')
        .map(|(target, rest)| (target.trim(), Some(rest.trim())))
        .unwrap_or((trimmed, None));
    let value = target_raw.to_ascii_lowercase();
    let target = match value.as_str() {
        "origin" => hermes_cron::DeliverTarget::Origin,
        "local" => hermes_cron::DeliverTarget::Local,
        "telegram" => hermes_cron::DeliverTarget::Telegram,
        "discord" => hermes_cron::DeliverTarget::Discord,
        "slack" => hermes_cron::DeliverTarget::Slack,
        "email" => hermes_cron::DeliverTarget::Email,
        "whatsapp" => hermes_cron::DeliverTarget::WhatsApp,
        "signal" => hermes_cron::DeliverTarget::Signal,
        "matrix" => hermes_cron::DeliverTarget::Matrix,
        "mattermost" => hermes_cron::DeliverTarget::Mattermost,
        "dingtalk" => hermes_cron::DeliverTarget::DingTalk,
        "feishu" => hermes_cron::DeliverTarget::Feishu,
        "wecom" => hermes_cron::DeliverTarget::WeCom,
        "weixin" | "wechat" | "wx" => hermes_cron::DeliverTarget::Weixin,
        "bluebubbles" | "imessage" => hermes_cron::DeliverTarget::BlueBubbles,
        "sms" => hermes_cron::DeliverTarget::Sms,
        "homeassistant" | "ha" => hermes_cron::DeliverTarget::HomeAssistant,
        "ntfy" => hermes_cron::DeliverTarget::Ntfy,
        _ => return None,
    };
    let platform = deliver_chat_id
        .or(inline_chat_id)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Some(hermes_cron::DeliverConfig { target, platform })
}

#[allow(clippy::too_many_arguments)]
async fn run_cron(
    cli: Cli,
    action: Option<String>,
    job_id: Option<String>,
    id: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    name: Option<String>,
    deliver: Option<String>,
    deliver_chat_id: Option<String>,
    repeat: Option<u32>,
    skills: Vec<String>,
    add_skills: Vec<String>,
    remove_skills: Vec<String>,
    clear_skills: bool,
    script: Option<String>,
    no_agent: bool,
    agent: bool,
    script_timeout_seconds: Option<u64>,
    script_shell: Option<String>,
    workdir: Option<String>,
    all: bool,
) -> Result<(), AgentError> {
    let data_dir = hermes_state_root(&cli).join("cron");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| AgentError::Io(format!("cron dir {}: {}", data_dir.display(), e)))?;
    let sched = cron_scheduler_for_data_dir(data_dir.clone());
    sched.load_persisted_jobs().await.map_err(cron_cli_error)?;
    let resolved_id = job_id.or(id).filter(|s| !s.trim().is_empty());

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let mut jobs = sched.list_jobs().await;
            jobs.sort_by(|a, b| a.id.cmp(&b.id));
            if jobs.is_empty() {
                println!("(no cron jobs in {})", data_dir.display());
                return Ok(());
            }
            println!("Cron jobs ({}):", data_dir.display());
            for j in jobs {
                if !all && matches!(j.status, hermes_cron::JobStatus::Completed) {
                    continue;
                }
                let snippet: String = j.prompt.chars().take(48).collect();
                println!(
                    "  {}  [{}]  {:?}  next_run={:?}  {}",
                    j.id, j.schedule, j.status, j.next_run, snippet
                );
            }
        }
        "create" | "add" => {
            let schedule = schedule.unwrap_or_else(|| "0 * * * *".to_string());
            let prompt = match prompt {
                Some(p) => p,
                None if no_agent => "[script-only cron job]".to_string(),
                None => {
                    return Err(AgentError::Config(
                        "cron create: use --prompt \"...\" (or pass --no-agent with --script)"
                            .into(),
                    ));
                }
            };
            let mut job = hermes_cron::CronJob::new(schedule, prompt);
            if let Some(name) = name.filter(|s| !s.trim().is_empty()) {
                job.name = Some(name);
            }
            if let Some(raw) = deliver.as_deref() {
                if let Some(cfg) = parse_deliver_config(raw, deliver_chat_id.as_deref()) {
                    job.deliver = Some(cfg);
                } else {
                    return Err(AgentError::Config(format!(
                        "Unknown deliver target '{}'",
                        raw
                    )));
                }
            }
            if let Some(repeat) = repeat {
                job.repeat = Some(repeat);
            }
            if !skills.is_empty() {
                job.skills = Some(skills.clone());
            }
            if let Some(script) = script {
                if !script.trim().is_empty() {
                    job.script = Some(script);
                }
            }
            if no_agent {
                job.no_agent = true;
            }
            if agent {
                job.no_agent = false;
            }
            if job.no_agent && job.script.as_ref().map_or(true, |s| s.trim().is_empty()) {
                return Err(AgentError::Config(
                    "cron create: --no-agent requires --script".into(),
                ));
            }
            if let Some(timeout_secs) = script_timeout_seconds.filter(|v| *v > 0) {
                job.script_timeout_seconds = Some(timeout_secs);
            }
            if let Some(shell) = script_shell.filter(|v| !v.trim().is_empty()) {
                job.script_shell = Some(shell.trim().to_string());
            }
            if let Some(workdir) = workdir {
                job.workdir = Some(workdir);
            }
            let jid = sched.create_job(job).await.map_err(cron_cli_error)?;
            println!(
                "Created cron job id={} (persisted under {})",
                jid,
                data_dir.display()
            );
        }
        "edit" => {
            let jid = resolved_id
                .ok_or_else(|| AgentError::Config("cron edit: use <job-id> or --id".into()))?;
            let mut job = sched
                .get_job(&jid)
                .await
                .ok_or_else(|| AgentError::Config(format!("unknown job id: {}", jid)))?;

            if let Some(schedule) = schedule {
                job.schedule = schedule;
                job.next_run = None;
            }
            if let Some(prompt) = prompt {
                job.prompt = prompt;
            }
            if let Some(name) = name {
                job.name = if name.trim().is_empty() {
                    None
                } else {
                    Some(name)
                };
            }
            if let Some(raw) = deliver.as_deref() {
                if let Some(cfg) = parse_deliver_config(raw, deliver_chat_id.as_deref()) {
                    job.deliver = Some(cfg);
                } else {
                    return Err(AgentError::Config(format!(
                        "Unknown deliver target '{}'",
                        raw
                    )));
                }
            }
            if let Some(repeat) = repeat {
                job.repeat = Some(repeat);
            }
            if !skills.is_empty() {
                job.skills = Some(skills.clone());
            }
            if clear_skills {
                job.skills = None;
            }
            if !add_skills.is_empty() {
                let mut current = job.skills.take().unwrap_or_default();
                for skill in add_skills {
                    if !current.iter().any(|s| s == &skill) {
                        current.push(skill);
                    }
                }
                job.skills = Some(current);
            }
            if !remove_skills.is_empty() {
                let mut current = job.skills.take().unwrap_or_default();
                current.retain(|s| !remove_skills.iter().any(|r| r == s));
                job.skills = if current.is_empty() {
                    None
                } else {
                    Some(current)
                };
            }
            if let Some(script) = script {
                if script.trim().is_empty() {
                    job.script = None;
                } else {
                    job.script = Some(script);
                }
            }
            if no_agent {
                job.no_agent = true;
            }
            if agent {
                job.no_agent = false;
            }
            if let Some(timeout_secs) = script_timeout_seconds {
                job.script_timeout_seconds = if timeout_secs == 0 {
                    None
                } else {
                    Some(timeout_secs)
                };
            }
            if let Some(shell) = script_shell {
                if shell.trim().is_empty() {
                    job.script_shell = None;
                } else {
                    job.script_shell = Some(shell.trim().to_string());
                }
            }
            if let Some(workdir) = workdir {
                if workdir.trim().is_empty() {
                    job.workdir = None;
                } else {
                    job.workdir = Some(workdir);
                }
            }
            if job.no_agent && job.script.as_ref().map_or(true, |s| s.trim().is_empty()) {
                return Err(AgentError::Config(
                    "cron edit: no_agent mode requires a non-empty script".into(),
                ));
            }
            sched.update_job(&jid, job).await.map_err(cron_cli_error)?;
            println!("Updated job {}", jid);
        }
        "delete" | "remove" | "pause" | "resume" | "run" | "history" => {
            let act = action.as_deref().unwrap_or("cron");
            let jid = resolved_id
                .ok_or_else(|| AgentError::Config(format!("{}: use <job-id> or --id", act)))?;
            match act {
                "delete" | "remove" => {
                    sched.remove_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Deleted job {}", jid);
                }
                "pause" => {
                    sched.pause_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Paused job {}", jid);
                }
                "resume" => {
                    sched.resume_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Resumed job {}", jid);
                }
                "run" => {
                    let live_sched = build_live_cron_scheduler(&cli, &data_dir)?;
                    live_sched
                        .load_persisted_jobs()
                        .await
                        .map_err(cron_cli_error)?;
                    let result = live_sched.run_job(&jid).await.map_err(cron_cli_error)?;
                    let json = serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| format!("{result:#?}"));
                    println!("{}", json);
                }
                "history" => {
                    let job = sched
                        .get_job(&jid)
                        .await
                        .ok_or_else(|| AgentError::Config(format!("unknown job id: {}", jid)))?;
                    let json = serde_json::to_string_pretty(&job)
                        .map_err(|e| AgentError::Config(e.to_string()))?;
                    println!("{}", json);
                }
                _ => {
                    return Err(AgentError::Config(format!(
                        "internal: unexpected cron action '{}'",
                        act
                    )));
                }
            }
        }
        "status" => {
            let jobs = sched.list_jobs().await;
            let active = jobs
                .iter()
                .filter(|j| matches!(j.status, hermes_cron::JobStatus::Active))
                .count();
            println!(
                "Cron scheduler status: jobs_total={} active={} data_dir={}",
                jobs.len(),
                active,
                data_dir.display()
            );
        }
        "tick" => {
            let now = chrono::Utc::now();
            let due: Vec<String> = sched
                .list_jobs()
                .await
                .into_iter()
                .filter(|j| j.is_due(now))
                .map(|j| j.id)
                .collect();
            if due.is_empty() {
                println!("No due jobs at {}.", now);
                return Ok(());
            }
            let live_sched = build_live_cron_scheduler(&cli, &data_dir)?;
            live_sched
                .load_persisted_jobs()
                .await
                .map_err(cron_cli_error)?;
            for jid in &due {
                let result = live_sched.run_job(jid).await;
                match result {
                    Ok(_) => println!("tick: ran {}", jid),
                    Err(e) => println!("tick: {} failed ({})", jid, e),
                }
            }
            println!("Tick complete: {} job(s) processed.", due.len());
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown cron action: {} (use list|create|edit|pause|resume|run|remove|delete|history|status|tick)",
                other
            )));
        }
    }
    Ok(())
}

fn webhook_store_path(cli: &Cli) -> PathBuf {
    hermes_state_root(&cli).join("webhooks.json")
}

fn webhook_subscriptions_path(cli: &Cli) -> PathBuf {
    hermes_state_root(&cli).join("webhook_subscriptions.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WebhookSubscription {
    #[serde(default)]
    description: String,
    #[serde(default)]
    events: Vec<String>,
    secret: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default = "default_webhook_deliver")]
    deliver: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deliver_extra: Option<serde_json::Value>,
    #[serde(default)]
    deliver_only: bool,
}

fn default_webhook_deliver() -> String {
    "log".to_string()
}

fn load_webhook_subscriptions(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, WebhookSubscription>, AgentError> {
    if !path.exists() {
        return Ok(std::collections::BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_webhook_subscriptions(
    path: &Path,
    subs: &std::collections::BTreeMap<String, WebhookSubscription>,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(subs).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

async fn prompt_line(prompt: impl Into<String>) -> Result<String, AgentError> {
    let prompt = prompt.into();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{self, Write};
        print!("{}", prompt);
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {}", e)))?
    .map_err(|e| AgentError::Io(format!("stdin: {}", e)))?;
    Ok(line.trim().to_string())
}

/// Resolve API key for `hermes auth login <provider>`: env → merged config → stdin.
async fn resolve_llm_login_token(cli: &Cli, provider: &str) -> Result<String, AgentError> {
    if let Some(k) = provider_api_key_from_env(provider) {
        return Ok(k);
    }
    let vault_path = secret_vault_path_for_cli(cli);
    if vault_path.exists() {
        let store = FileTokenStore::new(vault_path).await?;
        if let Some((_provider, token)) = lookup_secret_from_vault(&store, provider).await {
            return Ok(token);
        }
    }
    let cfg =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    if let Some(k) = cfg
        .llm_providers
        .get(provider)
        .and_then(|c| c.api_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(k.to_string());
    }
    let fallback_var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    let msg = format!(
        "No API key in env or config for provider '{}'.\n\
         Set {} (or `hermes secrets set {}`; plaintext fallback: `hermes config set llm.{}.api_key ...`) or paste key now: ",
        provider, fallback_var, provider, provider
    );
    let pasted = prompt_line(msg).await?;
    if pasted.is_empty() {
        return Err(AgentError::Config(format!(
            "Missing API key for provider '{}'",
            provider
        )));
    }
    Ok(pasted)
}

async fn run_webhook(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    url: Option<String>,
    id: Option<String>,
    prompt: Option<String>,
    events: Option<String>,
    description: Option<String>,
    skills: Option<String>,
    deliver: Option<String>,
    deliver_chat_id: Option<String>,
    secret: Option<String>,
    deliver_only: bool,
    payload: Option<String>,
) -> Result<(), AgentError> {
    let path = webhook_store_path(&cli);
    let mut store = hermes_cli::webhook_delivery::load_webhook_store(&path)?;
    let subs_path = webhook_subscriptions_path(&cli);
    let mut subs = load_webhook_subscriptions(&subs_path)?;

    match action.as_deref().unwrap_or("list") {
        "list" | "ls" => {
            if !subs.is_empty() {
                println!("Webhook subscriptions ({}):", subs_path.display());
                for (route, cfg) in &subs {
                    let events = if cfg.events.is_empty() {
                        "(all)".to_string()
                    } else {
                        cfg.events.join(", ")
                    };
                    println!(
                        "  {}  deliver={}  events={}  created_at={}",
                        route, cfg.deliver, events, cfg.created_at
                    );
                }
                println!();
            }
            if store.webhooks.is_empty() {
                println!("(no webhooks in {})", path.display());
                return Ok(());
            }
            println!("Webhooks ({}):", path.display());
            for w in &store.webhooks {
                println!("  {}  {}  {}", w.id, w.url, w.created_at);
            }
        }
        "subscribe" => {
            let route = name
                .ok_or_else(|| AgentError::Config("webhook subscribe: missing route name".into()))?
                .trim()
                .to_ascii_lowercase()
                .replace(' ', "-");
            if route.is_empty() {
                return Err(AgentError::Config(
                    "webhook subscribe: route name cannot be empty".into(),
                ));
            }
            let secret = secret.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let events_vec = events
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            let skills_vec = skills
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            let deliver = deliver.unwrap_or_else(|| "log".to_string());
            if deliver_only && deliver == "log" {
                return Err(AgentError::Config(
                    "--deliver-only requires --deliver to be a real target (not log)".into(),
                ));
            }
            let mut deliver_extra = None;
            if let Some(chat_id) = deliver_chat_id.filter(|s| !s.trim().is_empty()) {
                deliver_extra = Some(serde_json::json!({ "chat_id": chat_id }));
            }
            let sub = WebhookSubscription {
                description: description
                    .unwrap_or_else(|| format!("Agent-created subscription: {route}")),
                events: events_vec,
                secret: secret.clone(),
                prompt: prompt.unwrap_or_default(),
                skills: skills_vec,
                deliver: deliver.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                deliver_extra,
                deliver_only,
            };
            subs.insert(route.clone(), sub);
            save_webhook_subscriptions(&subs_path, &subs)?;
            println!("Created webhook subscription: {}", route);
            println!("  URL path: /webhooks/{}", route);
            if secret_stdout_allowed() {
                println!("  Secret: {}", secret);
                println!("  (plaintext output enabled via HERMES_ALLOW_SECRET_STDOUT=1)");
            } else {
                println!("  Secret: {}", mask_secret(&secret));
                println!("  (set HERMES_ALLOW_SECRET_STDOUT=1 to reveal plaintext once)");
            }
            println!("  Deliver: {}", deliver);
        }
        "add" => {
            let url = url
                .filter(|u| !u.is_empty())
                .ok_or_else(|| AgentError::Config("webhook add: use --url https://...".into()))?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(AgentError::Config(
                    "webhook URL must start with http:// or https://".into(),
                ));
            }
            let rec = hermes_cli::webhook_delivery::WebhookRecord {
                id: uuid::Uuid::new_v4().to_string(),
                url: url.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            store.webhooks.push(rec.clone());
            hermes_cli::webhook_delivery::save_webhook_store(&path, &store)?;
            println!("Added webhook {} -> {}", rec.id, rec.url);
        }
        "remove" | "rm" => {
            if let Some(route) = name.filter(|s| !s.trim().is_empty()) {
                if subs.remove(&route).is_some() {
                    save_webhook_subscriptions(&subs_path, &subs)?;
                    println!("Removed subscription '{}'.", route);
                    return Ok(());
                }
            }
            let before = store.webhooks.len();
            if let Some(rid) = id.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.id != rid);
            } else if let Some(u) = url.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.url != u);
            } else {
                return Err(AgentError::Config(
                    "webhook remove: use <name>, --id <id>, or --url <exact-url>".into(),
                ));
            }
            if store.webhooks.len() == before {
                println!("No matching webhook removed.");
            } else {
                hermes_cli::webhook_delivery::save_webhook_store(&path, &store)?;
                println!("Updated {}", path.display());
            }
        }
        "test" => {
            let route = name.ok_or_else(|| {
                AgentError::Config("webhook test: usage `hermes webhook test <name>`".into())
            })?;
            let sub = subs
                .get(&route)
                .ok_or_else(|| AgentError::Config(format!("No subscription named '{}'.", route)))?;
            let body = payload.unwrap_or_else(|| {
                r#"{"test":true,"event_type":"test","message":"Hello from hermes webhook test"}"#
                    .to_string()
            });
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(sub.secret.as_bytes())
                .map_err(|e| AgentError::Config(format!("webhook hmac key: {e}")))?;
            use hmac::Mac;
            mac.update(body.as_bytes());
            let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
            let cfg = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            let webhook_cfg = cfg.platforms.get("webhook");
            let host = webhook_cfg
                .and_then(|p| p.extra.get("host"))
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1");
            let port = webhook_cfg
                .and_then(|p| p.extra.get("port"))
                .and_then(|v| v.as_u64())
                .unwrap_or(8644);
            let display_host = if host == "0.0.0.0" { "127.0.0.1" } else { host };
            let target_url = format!("http://{}:{}/webhooks/{}", display_host, port, route);
            let client = reqwest::Client::new();
            let resp = client
                .post(&target_url)
                .header("Content-Type", "application/json")
                .header("X-Hub-Signature-256", sig)
                .header("X-GitHub-Event", "test")
                .body(body)
                .send()
                .await
                .map_err(|e| AgentError::Io(format!("webhook test send: {}", e)))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            println!("Test POST {} -> {}", target_url, status);
            if !text.trim().is_empty() {
                println!("{}", text);
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown webhook action: {} (use subscribe|add|list|remove|test)",
                other
            )));
        }
    }
    Ok(())
}

/// POST each [`CronCompletionEvent`] to every URL in `webhooks.json` (same file as `hermes webhook`).
async fn run_cron_webhook_delivery_loop(
    mut rx: broadcast::Receiver<CronCompletionEvent>,
    webhooks_json: PathBuf,
) {
    use tokio::sync::broadcast::error::RecvError;

    let client = match hermes_cli::webhook_delivery::webhook_http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("cron webhooks: HTTP client build failed: {e}");
            return;
        }
    };

    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(n)) => {
                tracing::debug!(n, "cron webhook receiver lagged; skipped messages");
                continue;
            }
            Err(RecvError::Closed) => break,
        };

        if let Err(e) = hermes_cli::webhook_delivery::deliver_cron_completion_to_webhooks(
            &webhooks_json,
            &ev,
            &client,
        )
        .await
        {
            tracing::warn!("cron webhook delivery: {e}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronPlatformDeliveryTarget {
    platform: &'static str,
    chat_id: String,
    thread_id: Option<String>,
}

fn cron_deliver_target_platform_name(target: &DeliverTarget) -> Option<&'static str> {
    match target {
        DeliverTarget::Origin | DeliverTarget::Local => None,
        DeliverTarget::Telegram => Some("telegram"),
        DeliverTarget::Discord => Some("discord"),
        DeliverTarget::Slack => Some("slack"),
        DeliverTarget::Email => Some("email"),
        DeliverTarget::WhatsApp => Some("whatsapp"),
        DeliverTarget::Signal => Some("signal"),
        DeliverTarget::Matrix => Some("matrix"),
        DeliverTarget::Mattermost => Some("mattermost"),
        DeliverTarget::DingTalk => Some("dingtalk"),
        DeliverTarget::Feishu => Some("feishu"),
        DeliverTarget::WeCom => Some("wecom"),
        DeliverTarget::Weixin => Some("weixin"),
        DeliverTarget::BlueBubbles => Some("bluebubbles"),
        DeliverTarget::Sms => Some("sms"),
        DeliverTarget::HomeAssistant => Some("homeassistant"),
        DeliverTarget::Ntfy => Some("ntfy"),
    }
}

fn cron_home_channel_env_vars(platform: &str) -> &'static [&'static str] {
    match platform {
        "telegram" => &["TELEGRAM_HOME_CHANNEL"],
        "discord" => &["DISCORD_HOME_CHANNEL"],
        "slack" => &["SLACK_HOME_CHANNEL"],
        "email" => &["EMAIL_HOME_CHANNEL"],
        "whatsapp" => &["WHATSAPP_HOME_CHANNEL"],
        "signal" => &["SIGNAL_HOME_CHANNEL"],
        "matrix" => &["MATRIX_HOME_ROOM", "MATRIX_HOME_CHANNEL"],
        "mattermost" => &["MATTERMOST_HOME_CHANNEL"],
        "dingtalk" => &["DINGTALK_HOME_CHANNEL"],
        "feishu" => &["FEISHU_HOME_CHANNEL"],
        "wecom" => &["WECOM_HOME_CHANNEL"],
        "weixin" => &["WEIXIN_HOME_CHANNEL"],
        "bluebubbles" => &["BLUEBUBBLES_HOME_CHANNEL"],
        "sms" => &["SMS_HOME_CHANNEL"],
        "homeassistant" => &["HOMEASSISTANT_HOME_CHANNEL"],
        "ntfy" => &["NTFY_HOME_CHANNEL"],
        _ => &[],
    }
}

fn cron_home_channel_for_platform(platform: &str) -> Option<String> {
    cron_home_channel_env_vars(platform)
        .iter()
        .find_map(|key| env_string(key))
}

fn split_telegram_cron_target(chat_id: &str) -> (String, Option<String>) {
    let chat_id = chat_id.trim();
    if let Some((base, suffix)) = chat_id.rsplit_once(':') {
        let base = base.trim();
        let suffix = suffix.trim();
        if !base.is_empty() && suffix.parse::<i64>().is_ok() {
            return (base.to_string(), Some(suffix.to_string()));
        }
    }
    let thread_id = env_string("TELEGRAM_CRON_THREAD_ID");
    (chat_id.to_string(), thread_id)
}

fn cron_platform_delivery_target(
    event: &CronCompletionEvent,
) -> Option<CronPlatformDeliveryTarget> {
    let deliver = event.deliver.as_ref()?;
    let platform = cron_deliver_target_platform_name(&deliver.target)?;
    let raw_chat_id = deliver
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| cron_home_channel_for_platform(platform))?;

    let (chat_id, thread_id) = if platform == "telegram" {
        split_telegram_cron_target(&raw_chat_id)
    } else {
        (raw_chat_id.trim().to_string(), None)
    };

    (!chat_id.trim().is_empty()).then_some(CronPlatformDeliveryTarget {
        platform,
        chat_id,
        thread_id,
    })
}

fn cron_platform_delivery_text(event: &CronCompletionEvent) -> Option<String> {
    if event.ok {
        let text = event
            .assistant_output
            .as_deref()
            .or(event.assistant_snippet.as_deref())?;
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.trim_start().starts_with("[SILENT]") {
            return None;
        }
        return Some(trimmed.to_string());
    }

    let name = event
        .job_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&event.job_id);
    let error = event
        .error
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown cron failure");
    Some(format!("Cron job '{name}' failed:\n{error}"))
}

async fn run_cron_gateway_delivery_loop(
    mut rx: broadcast::Receiver<CronCompletionEvent>,
    gateway: Arc<Gateway>,
) {
    use tokio::sync::broadcast::error::RecvError;

    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(n)) => {
                tracing::debug!(n, "cron gateway delivery receiver lagged; skipped messages");
                continue;
            }
            Err(RecvError::Closed) => break,
        };

        let Some(target) = cron_platform_delivery_target(&event) else {
            continue;
        };
        let Some(text) = cron_platform_delivery_text(&event) else {
            tracing::debug!(
                job_id = %event.job_id,
                "cron gateway delivery skipped empty completion text"
            );
            continue;
        };

        if let Err(err) = gateway
            .send_message_explicit_with_audit_label(
                target.platform,
                &target.chat_id,
                &text,
                None,
                target.thread_id.as_deref(),
                Some(&event.job_id),
            )
            .await
        {
            tracing::warn!(
                job_id = %event.job_id,
                platform = target.platform,
                chat_id = %target.chat_id,
                thread_id = ?target.thread_id,
                "cron gateway delivery failed: {err}"
            );
        }
    }
}

async fn run_dump(
    cli: Cli,
    session: Option<String>,
    output: Option<String>,
) -> Result<(), AgentError> {
    let home = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let requested = session.as_deref();
    let payload = load_resume_payload(&cli, requested)?;
    let out = output.map(PathBuf::from).unwrap_or_else(|| {
        home.join("sessions").join("saved").join(format!(
            "hermes_conversation_{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create dump output directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    let system_prompt = payload
        .system_prompt
        .clone()
        .or_else(|| leading_system_prompt_for_persist(&payload.messages));
    let payload = serde_json::json!({
        "session_id": payload.session_id,
        "resolved_id": payload.resolved_id,
        "source_path": payload.source_path,
        "model": payload.model,
        "personality": payload.personality,
        "system_prompt": system_prompt,
        "session_start": payload.session_start,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "messages": payload.messages,
    });
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write dump: {}", e)))?;
    println!("Wrote dump to {}", out.display());
    Ok(())
}

fn run_completion(shell: Option<String>) -> Result<(), AgentError> {
    let mut cmd = Cli::command();
    let sh = match shell.as_deref().unwrap_or("zsh") {
        "bash" => CompletionShell::Bash,
        "fish" => CompletionShell::Fish,
        "powershell" => CompletionShell::PowerShell,
        "elvish" => CompletionShell::Elvish,
        _ => CompletionShell::Zsh,
    };
    generate(sh, &mut cmd, "hermes-agent-ultra", &mut std::io::stdout());
    Ok(())
}

async fn run_uninstall(yes: bool) -> Result<(), AgentError> {
    let home = hermes_config::hermes_home();
    if !yes {
        println!("Uninstall is destructive. Re-run with `hermes uninstall --yes`.");
        return Ok(());
    }
    if home.exists() {
        std::fs::remove_dir_all(&home)
            .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", home.display(), e)))?;
        println!("Removed {}", home.display());
    } else {
        println!("Nothing to uninstall.");
    }
    Ok(())
}

/// Handle `hermes lumio [action]`.
async fn run_lumio(action: Option<String>, model: Option<String>) -> Result<(), AgentError> {
    match action.as_deref() {
        None | Some("login") => {
            hermes_cli::lumio::setup(model.as_deref(), true).await?;
        }
        Some("logout") => {
            hermes_cli::lumio::clear_token();
            println!("✅ Lumio token removed.");
        }
        Some("status") => match hermes_cli::lumio::load_token() {
            Some(t) => {
                let user = if t.username.is_empty() {
                    "(unknown)"
                } else {
                    &t.username
                };
                println!("Lumio: logged in as {}", user);
                println!("  API: {}", t.base_url);
                println!("  Token: {}", mask_secret(&t.token));
            }
            None => {
                println!("Lumio: not logged in");
                println!("  Run `hermes lumio` to login.");
            }
        },
        Some(other) => {
            println!(
                "Unknown lumio action: '{}'. Use: login, logout, status.",
                other
            );
        }
    }
    Ok(())
}

