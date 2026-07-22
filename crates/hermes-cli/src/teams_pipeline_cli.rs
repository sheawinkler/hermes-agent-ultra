//! Rust-native `hermes teams-pipeline` command surface.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use uuid::Uuid;
use hermes_config::{load_config, GatewayConfig};
use hermes_core::AgentError;
use hermes_tools::{
    default_change_type_for_resource, maintain_graph_subscriptions,
    resolve_teams_pipeline_store_path, sync_graph_subscription_record, MicrosoftGraphClient,
    MicrosoftGraphTeamsBackend, MicrosoftGraphTokenProvider, TeamsGraphBackend,
    TeamsMeetingPipeline, TeamsMeetingPipelineJob, TeamsPipelineConfig, TeamsPipelineError,
    TeamsPipelineStore,
};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Default)]
pub struct TeamsPipelineCliOptions {
    pub config_dir: Option<String>,
    pub action: Option<String>,
    pub id: Option<String>,
    pub limit: usize,
    pub status: Option<String>,
    pub store_path: Option<String>,
    pub meeting_id: Option<String>,
    /// Microsoft Entra object ID for the meeting organizer; required for Graph artifact access.
    pub organizer_user_id: Option<String>,
    pub join_web_url: Option<String>,
    pub tenant_id: Option<String>,
    pub call_record_id: Option<String>,
    pub resource: Option<String>,
    pub notification_url: Option<String>,
    pub change_type: Option<String>,
    pub expiration: Option<String>,
    pub client_state: Option<String>,
    pub lifecycle_notification_url: Option<String>,
    pub latest_supported_tls_version: String,
    pub force_refresh: bool,
    pub renew_within_hours: u32,
    pub extend_hours: u32,
    pub dry_run: bool,
}

pub async fn handle_cli_teams_pipeline(options: TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let action = options.action.as_deref().unwrap_or("list");
    match action {
        "list" | "ls" => cmd_list(&options),
        "show" => cmd_show(&options),
        "run" | "replay" => cmd_run(&options).await,
        "fetch" | "test" => cmd_fetch(&options).await,
        "subscriptions" | "subs" => cmd_subscriptions(&options).await,
        "subscribe" => cmd_subscribe(&options).await,
        "renew-subscription" => cmd_renew_subscription(&options).await,
        "delete-subscription" => cmd_delete_subscription(&options).await,
        "maintain-subscriptions" => cmd_maintain_subscriptions(&options).await,
        "token-health" | "token" => cmd_token_health(&options).await,
        "validate" => cmd_validate(&options),
        other => Err(AgentError::Config(format!(
            "Unknown teams-pipeline action: {other} (use list|show|run|fetch|subscriptions|subscribe|renew-subscription|delete-subscription|maintain-subscriptions|token-health|validate)"
        ))),
    }
}

fn cmd_list(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let store = open_store(options)?;
    let mut jobs = store
        .list_jobs()
        .map_err(agent_error)?
        .into_values()
        .collect::<Vec<_>>();
    if let Some(status) = options
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        jobs.retain(|job| {
            job.get("status")
                .and_then(Value::as_str)
                .map(|candidate| candidate.eq_ignore_ascii_case(status))
                .unwrap_or(false)
        });
    }
    jobs.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(a.get("updated_at").and_then(Value::as_str).unwrap_or(""))
    });
    let limit = if options.limit == 0 {
        20
    } else {
        options.limit
    };
    println!("\n{} Teams pipeline job(s):\n", jobs.len().min(limit));
    for job in jobs.into_iter().take(limit) {
        let job_id = job.get("job_id").and_then(Value::as_str).unwrap_or("-");
        let status = job.get("status").and_then(Value::as_str).unwrap_or("-");
        let updated = job.get("updated_at").and_then(Value::as_str).unwrap_or("-");
        let meeting = job
            .get("meeting_ref")
            .and_then(|v| v.get("meeting_id"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        println!("  {job_id}  {status}  meeting={meeting}  updated={updated}");
    }
    Ok(())
}

fn cmd_show(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let id = required_id(options, "teams-pipeline show <job-id>")?;
    let store = open_store(options)?;
    let job = store
        .get_job(&id)
        .map_err(agent_error)?
        .ok_or_else(|| AgentError::Config(format!("Unknown Teams pipeline job: {id}")))?;
    print_json(job)
}

async fn cmd_run(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let id = required_id(options, "teams-pipeline run <job-id>")?;
    let organizer_user_id = required_organizer_user_id(options, "teams-pipeline run")?;
    let pipeline = build_pipeline(options)?;
    let payload = pipeline
        .store()
        .get_job(&id)
        .map_err(agent_error)?
        .ok_or_else(|| AgentError::Config(format!("Unknown Teams pipeline job: {id}")))?;
    let mut job = TeamsMeetingPipelineJob::from_value(payload).map_err(agent_error)?;
    let meeting_ref = job.meeting_ref.as_mut().ok_or_else(|| {
        AgentError::Config(format!("Teams pipeline job {id} has no meeting reference."))
    })?;
    meeting_ref.organizer_user_id = Some(organizer_user_id);
    let job = pipeline.run_job_record(job).await.map_err(agent_error)?;
    print_json(compact_job(
        serde_json::to_value(job).map_err(json_agent_error)?,
    ))
}

async fn cmd_fetch(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let meeting_id = options
        .meeting_id
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let join_web_url = options
        .join_web_url
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    if meeting_id.is_none() && join_web_url.is_none() {
        return Err(AgentError::Config(
            "teams-pipeline fetch: meeting_id or join_web_url is required".into(),
        ));
    }
    let organizer_user_id = required_organizer_user_id(options, "teams-pipeline fetch")?;
    let backend = MicrosoftGraphTeamsBackend::from_env().map_err(agent_error)?;
    let meeting = backend
        .resolve_meeting_reference(
            meeting_id,
            Some(&organizer_user_id),
            join_web_url,
            options.tenant_id.as_deref(),
        )
        .await
        .map_err(agent_error)?;
    let transcript = backend
        .fetch_preferred_transcript_text(&meeting)
        .await
        .map_err(agent_error)?
        .map(|(artifact, text)| {
            json!({
                "artifact": artifact,
                "transcript_preview": text.chars().take(500).collect::<String>(),
                "transcript_chars": text.chars().count()
            })
        });
    let recordings = backend
        .list_recording_artifacts(&meeting)
        .await
        .map_err(agent_error)?;
    let call_record = backend
        .enrich_meeting_with_call_record(&meeting, options.call_record_id.as_deref())
        .await
        .map_err(agent_error)?;
    print_json(json!({
        "meeting_ref": meeting,
        "transcript": transcript,
        "recordings": recordings,
        "call_record": call_record
    }))
}

async fn cmd_subscriptions(_options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let client = MicrosoftGraphClient::from_env().map_err(agent_error)?;
    let subscriptions = client
        .collect_paginated("/subscriptions")
        .await
        .map_err(agent_error)?;
    if subscriptions.is_empty() {
        println!("No Microsoft Graph subscriptions found.");
        return Ok(());
    }
    println!(
        "\n{} Microsoft Graph subscription(s):\n",
        subscriptions.len()
    );
    for subscription in subscriptions {
        let id = subscription
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let resource = subscription
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let change_type = subscription
            .get("changeType")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let expiration = subscription
            .get("expirationDateTime")
            .and_then(Value::as_str)
            .unwrap_or("-");
        println!("  {id}  {change_type}  {resource}  expires={expiration}");
    }
    Ok(())
}

async fn cmd_subscribe(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let resource = options
        .resource
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::Config("teams-pipeline subscribe: --resource is required".into())
        })?;
    let notification_url = options
        .notification_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::Config("teams-pipeline subscribe: --notification-url is required".into())
        })?;
    let change_type = options
        .change_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_change_type_for_resource(resource));
    let expiration = options
        .expiration
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            (Utc::now() + ChronoDuration::hours(24)).to_rfc3339_opts(SecondsFormat::Secs, true)
        });

    let mut body = Map::new();
    body.insert("changeType".into(), Value::String(change_type.into()));
    body.insert(
        "notificationUrl".into(),
        Value::String(notification_url.into()),
    );
    body.insert("resource".into(), Value::String(resource.into()));
    body.insert("expirationDateTime".into(), Value::String(expiration));
    if let Some(client_state) = options
        .client_state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body.insert("clientState".into(), Value::String(client_state.into()));
    }
    if let Some(lifecycle_url) = options
        .lifecycle_notification_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body.insert(
            "lifecycleNotificationUrl".into(),
            Value::String(lifecycle_url.into()),
        );
    }
    if !options.latest_supported_tls_version.trim().is_empty() {
        body.insert(
            "latestSupportedTlsVersion".into(),
            Value::String(options.latest_supported_tls_version.clone()),
        );
    }

    let client = MicrosoftGraphClient::from_env().map_err(agent_error)?;
    let created = client
        .post_json("/subscriptions", Value::Object(body))
        .await
        .map_err(agent_error)?;
    let store = open_store(options)?;
    sync_graph_subscription_record(&store, created.clone(), Some("active"), false)
        .map_err(agent_error)?;
    print_json(created)
}

async fn cmd_renew_subscription(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let id = required_id(
        options,
        "teams-pipeline renew-subscription <subscription-id>",
    )?;
    let expiration = options
        .expiration
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AgentError::Config("teams-pipeline renew-subscription: --expiration is required".into())
        })?;
    let client = MicrosoftGraphClient::from_env().map_err(agent_error)?;
    let patched = client
        .patch_json(
            &format!("/subscriptions/{id}"),
            json!({"expirationDateTime": expiration}),
        )
        .await
        .map_err(agent_error)?;
    let store = open_store(options)?;
    sync_graph_subscription_record(&store, patched.clone(), Some("active"), true)
        .map_err(agent_error)?;
    print_json(patched)
}

async fn cmd_delete_subscription(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let id = required_id(
        options,
        "teams-pipeline delete-subscription <subscription-id>",
    )?;
    let client = MicrosoftGraphClient::from_env().map_err(agent_error)?;
    let result = client
        .delete_json(&format!("/subscriptions/{id}"))
        .await
        .map_err(agent_error)?;
    let store = open_store(options)?;
    let removed_local = store.delete_subscription(&id).map_err(agent_error)?;
    print_json(json!({"deleted": true, "removed_local": removed_local, "graph_result": result}))
}

async fn cmd_maintain_subscriptions(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let client = MicrosoftGraphClient::from_env().map_err(agent_error)?;
    let store = open_store(options)?;
    let result = maintain_graph_subscriptions(
        &client,
        &store,
        options.renew_within_hours,
        options.extend_hours,
        options.dry_run,
        options.client_state.as_deref(),
    )
    .await
    .map_err(agent_error)?;
    print_json(result)
}

async fn cmd_token_health(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let provider = MicrosoftGraphTokenProvider::from_env().map_err(agent_error)?;
    let mut payload = provider.inspect_token_health();
    if options.force_refresh {
        match provider.get_access_token(true).await {
            Ok(token) => {
                payload["last_refresh_succeeded"] = Value::Bool(true);
                payload["access_token_length"] = json!(token.len());
            }
            Err(err) => {
                payload["last_refresh_succeeded"] = Value::Bool(false);
                payload["last_refresh_error"] = Value::String(err.to_string());
            }
        }
    }
    print_json(payload)
}

fn cmd_validate(options: &TeamsPipelineCliOptions) -> Result<(), AgentError> {
    let store = open_store(options)?;
    let config = load_config(options.config_dir.as_deref())
        .map_err(|e| AgentError::Config(format!("load config: {e}")))?;
    let snapshot = validate_configuration_snapshot(&config, &store)?;
    print_json(snapshot)
}

/// Rust CLI only supports manual local transcript/summary output. External
/// Teams delivery is unsupported in this Rust release; the optional source
/// plugin is not packaged or verified.
pub fn build_pipeline_runtime_config(config: &GatewayConfig) -> Value {
    config
        .platforms
        .get("teams")
        .and_then(|teams| teams.extra.get("meeting_pipeline"))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn validate_configuration_snapshot(
    config: &GatewayConfig,
    store: &TeamsPipelineStore,
) -> Result<Value, AgentError> {
    let mut issues = Vec::new();
    let graph = json!({
        "tenant_id": std::env::var("MSGRAPH_TENANT_ID").ok().map(|s| !s.trim().is_empty()).unwrap_or(false),
        "client_id": std::env::var("MSGRAPH_CLIENT_ID").ok().map(|s| !s.trim().is_empty()).unwrap_or(false),
        "client_secret": std::env::var("MSGRAPH_CLIENT_SECRET").ok().map(|s| !s.trim().is_empty()).unwrap_or(false),
    });
    if !graph["tenant_id"].as_bool().unwrap_or(false)
        || !graph["client_id"].as_bool().unwrap_or(false)
        || !graph["client_secret"].as_bool().unwrap_or(false)
    {
        issues.push("Microsoft Graph app-only credentials are incomplete.".to_owned());
    }
    if let Err(error) =
        TeamsPipelineConfig::from_value(Some(build_pipeline_runtime_config(config)))
    {
        issues.push(error.to_string());
    }
    Ok(json!({
        "ok": issues.is_empty(),
        "issues": issues,
        "graph_config": graph,
        "runtime": "manual_local_only",
        "external_sinks": "unsupported in this Rust release; optional source plugin is not packaged or verified",
        "store_path": store.path(),
        "store_stats": store.stats().map_err(agent_error)?,
    }))
}


fn build_pipeline(options: &TeamsPipelineCliOptions) -> Result<TeamsMeetingPipeline, AgentError> {
    let store = Arc::new(open_store(options)?);
    let backend = Arc::new(MicrosoftGraphTeamsBackend::from_env().map_err(agent_error)?);
    let config = match load_config(options.config_dir.as_deref()) {
        Ok(config) => TeamsPipelineConfig::from_value(Some(build_pipeline_runtime_config(&config)))
            .map_err(agent_error)?,
        Err(_) => TeamsPipelineConfig::default(),
    };
    Ok(TeamsMeetingPipeline::new(backend, store, config))
}

fn open_store(options: &TeamsPipelineCliOptions) -> Result<TeamsPipelineStore, AgentError> {
    let path = options
        .store_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let path = resolve_teams_pipeline_store_path(path.as_deref()).map_err(agent_error)?;
    TeamsPipelineStore::new(path).map_err(agent_error)
}

fn required_id(options: &TeamsPipelineCliOptions, usage: &str) -> Result<String, AgentError> {
    options
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AgentError::Config(format!("{usage}: missing id")))
}

fn required_organizer_user_id(
    options: &TeamsPipelineCliOptions,
    action: &str,
) -> Result<String, AgentError> {
    let raw = options
        .organizer_user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AgentError::Config(format!(
                "{action}: --organizer-user-id (Microsoft Entra object ID) is required"
            ))
        })?;
    Uuid::parse_str(raw).map(|identifier| identifier.to_string()).map_err(|_| {
        AgentError::Config(format!(
            "{action}: --organizer-user-id must be a Microsoft Entra object ID UUID"
        ))
    })
}

fn compact_job(mut value: Value) -> Value {
    if let Some(summary) = value
        .get_mut("summary_payload")
        .and_then(Value::as_object_mut)
    {
        if let Some(transcript) = summary
            .remove("transcript_text")
            .and_then(|v| v.as_str().map(|s| s.chars().take(240).collect::<String>()))
        {
            summary.insert("transcript_preview".into(), Value::String(transcript));
        }
    }
    value
}

fn print_json(value: Value) -> Result<(), AgentError> {
    let raw = serde_json::to_string_pretty(&value).map_err(json_agent_error)?;
    println!("{raw}");
    Ok(())
}

fn json_agent_error(error: serde_json::Error) -> AgentError {
    AgentError::Config(format!("json: {error}"))
}

fn agent_error(error: TeamsPipelineError) -> AgentError {
    match error {
        TeamsPipelineError::Config(message)
        | TeamsPipelineError::Invalid(message)
        | TeamsPipelineError::Store(message)
        | TeamsPipelineError::Json(message) => AgentError::Config(message),
        TeamsPipelineError::Io(message) => AgentError::Io(message),
        TeamsPipelineError::Retryable(message) | TeamsPipelineError::ArtifactNotFound(message) => {
            AgentError::ToolExecution(message)
        }
        TeamsPipelineError::Graph {
            status, message, ..
        } => {
            AgentError::ToolExecution(format!("Microsoft Graph HTTP {status}: {message}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::PlatformConfig;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_environment() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }


    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.as_deref() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn runtime_config_uses_only_explicit_manual_pipeline_settings() {
        let mut config = GatewayConfig::default();
        let mut teams = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        teams.extra.insert("delivery_mode".into(), json!("graph"));
        teams.extra.insert("team_id".into(), json!("team-1"));
        teams.extra.insert("channel_id".into(), json!("channel-1"));
        teams.extra.insert(
            "meeting_pipeline".into(),
            json!({"transcript_min_chars": 120}),
        );
        config.platforms.insert("teams".into(), teams);

        let runtime_config = build_pipeline_runtime_config(&config);

        assert_eq!(runtime_config, json!({"transcript_min_chars": 120}));
    }

    #[test]
    fn compact_job_hides_full_transcript() {
        let compact = compact_job(json!({
            "summary_payload": {
                "summary": "ok",
                "transcript_text": "abcdef"
            }
        }));
        assert!(compact["summary_payload"].get("transcript_text").is_none());
        assert_eq!(
            compact["summary_payload"]["transcript_preview"],
            json!("abcdef")
        );
    }

    #[tokio::test]
    async fn fetch_rejects_missing_blank_or_malformed_organizer_before_network() {
        let _env_lock = lock_environment();
        let server = MockServer::start().await;
        let _base = EnvVarGuard::set("MSGRAPH_BASE_URL", &server.uri());
        let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
        for organizer_user_id in [None, Some("   "), Some("not-an-entra-object-id")] {
            let options = TeamsPipelineCliOptions {
                meeting_id: Some("meeting-cli".into()),
                id: Some("job-cli".into()),
                organizer_user_id: organizer_user_id.map(ToOwned::to_owned),
                ..TeamsPipelineCliOptions::default()
            };
            let error = cmd_fetch(&options).await.unwrap_err();
            assert!(
                error.to_string().contains("--organizer-user-id"),
                "unexpected error: {error}"
            );
            let error = cmd_run(&options).await.unwrap_err();
            assert!(
                error.to_string().contains("--organizer-user-id"),
                "unexpected error: {error}"
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fetch_uses_exact_user_scoped_graph_paths() {
        let _env_lock = lock_environment();
        let server = MockServer::start().await;
        let organizer = "11111111-1111-1111-1111-111111111111";
        let meeting_id = "meeting-cli";
        let meeting_path = format!("/users/{organizer}/onlineMeetings/{meeting_id}");
        let transcripts_path = format!("{meeting_path}/transcripts");
        let recordings_path = format!("{meeting_path}/recordings");
        Mock::given(method("GET"))
            .and(path(meeting_path.as_str()))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": meeting_id,
                "organizer": {"identity": {"user": {"id": organizer}}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        for resource_path in [&transcripts_path, &recordings_path] {
            Mock::given(method("GET"))
                .and(path(resource_path.as_str()))
                .and(header("authorization", "Bearer test-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
                .expect(1)
                .mount(&server)
                .await;
        }
        let _base = EnvVarGuard::set("MSGRAPH_BASE_URL", &server.uri());
        let _token = EnvVarGuard::set("MSGRAPH_ACCESS_TOKEN", "test-token");
        let options = TeamsPipelineCliOptions {
            meeting_id: Some(meeting_id.into()),
            organizer_user_id: Some(organizer.into()),
            ..TeamsPipelineCliOptions::default()
        };

        cmd_fetch(&options).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].url.path(), meeting_path);
        assert_eq!(requests[1].url.path(), transcripts_path);
        assert_eq!(requests[2].url.path(), recordings_path);
    }
}
