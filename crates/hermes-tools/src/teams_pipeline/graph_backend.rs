impl MicrosoftGraphTokenProvider {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        let config = MicrosoftGraphAuthConfig {
            direct_access_token: env_nonempty("MSGRAPH_ACCESS_TOKEN")
                .or_else(|| env_nonempty("TEAMS_GRAPH_ACCESS_TOKEN")),
            tenant_id: env_nonempty("MSGRAPH_TENANT_ID"),
            client_id: env_nonempty("MSGRAPH_CLIENT_ID"),
            client_secret: env_nonempty("MSGRAPH_CLIENT_SECRET"),
            scope: env_nonempty("MSGRAPH_SCOPE")
                .unwrap_or_else(|| "https://graph.microsoft.com/.default".into()),
            authority_url: env_nonempty("MSGRAPH_AUTHORITY_URL")
                .unwrap_or_else(|| "https://login.microsoftonline.com".into()),
        };
        if config.direct_access_token.is_none()
            && (config.tenant_id.is_none()
                || config.client_id.is_none()
                || config.client_secret.is_none())
        {
            return Err(TeamsPipelineError::Config(
                "Microsoft Graph is not configured. Set MSGRAPH_TENANT_ID, MSGRAPH_CLIENT_ID, and MSGRAPH_CLIENT_SECRET.".into(),
            ));
        }
        Ok(Self {
            client: graph_http_client(),
            config,
            cache: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn inspect_token_health(&self) -> Value {
        json!({
            "configured": self.config.direct_access_token.is_some()
                || (self.config.tenant_id.is_some()
                    && self.config.client_id.is_some()
                    && self.config.client_secret.is_some()),
            "direct_access_token": self.config.direct_access_token.is_some(),
            "tenant_id": self.config.tenant_id.is_some(),
            "client_id": self.config.client_id.is_some(),
            "client_secret": self.config.client_secret.is_some(),
            "scope": self.config.scope,
            "authority_url": self.config.authority_url,
        })
    }

    pub async fn get_access_token(&self, force_refresh: bool) -> TeamsPipelineResult<String> {
        if let Some(token) = &self.config.direct_access_token {
            return Ok(token.clone());
        }
        if !force_refresh {
            if let Some(cached) = self.cache.lock().await.clone() {
                if cached.expires_at > Utc::now() + ChronoDuration::seconds(60) {
                    return Ok(cached.access_token);
                }
            }
        }
        let tenant =
            self.config.tenant_id.as_deref().ok_or_else(|| {
                TeamsPipelineError::Config("MSGRAPH_TENANT_ID is missing.".into())
            })?;
        let token_url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.config.authority_url.trim_end_matches('/'),
            path_percent_encode(tenant)
        );
        let response = self
            .client
            .post(token_url)
            .form(&[
                ("client_id", self.config.client_id.as_deref().unwrap_or("")),
                (
                    "client_secret",
                    self.config.client_secret.as_deref().unwrap_or(""),
                ),
                ("scope", self.config.scope.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Config(format!("Graph token request failed: {e}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(TeamsPipelineError::Config(format!(
                "Graph token request failed with HTTP {status}: {text}"
            )));
        }
        let payload: Value = serde_json::from_str(&text)?;
        let access_token = payload
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                TeamsPipelineError::Config("Graph token response omitted access_token.".into())
            })?
            .to_string();
        let expires_in = payload
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3600)
            .max(120);
        let cached = CachedGraphToken {
            access_token: access_token.clone(),
            expires_at: Utc::now() + ChronoDuration::seconds(expires_in - 60),
        };
        *self.cache.lock().await = Some(cached);
        Ok(access_token)
    }
}

#[derive(Debug, Clone)]
pub struct MicrosoftGraphClient {
    client: Client,
    token_provider: MicrosoftGraphTokenProvider,
    base_url: String,
}

impl MicrosoftGraphClient {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        Ok(Self {
            client: graph_http_client(),
            token_provider: MicrosoftGraphTokenProvider::from_env()?,
            base_url: env_nonempty("MSGRAPH_BASE_URL")
                .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into()),
        })
    }

    pub fn new(token_provider: MicrosoftGraphTokenProvider, base_url: impl Into<String>) -> Self {
        Self {
            client: graph_http_client(),
            token_provider,
            base_url: base_url.into(),
        }
    }

    pub fn token_provider(&self) -> &MicrosoftGraphTokenProvider {
        &self.token_provider
    }

    pub async fn get_json(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<Value> {
        self.request_json(Method::GET, path, params, None).await
    }

    pub async fn post_json(&self, path: &str, body: Value) -> TeamsPipelineResult<Value> {
        self.request_json(Method::POST, path, &[], Some(body)).await
    }

    pub async fn patch_json(&self, path: &str, body: Value) -> TeamsPipelineResult<Value> {
        self.request_json(Method::PATCH, path, &[], Some(body))
            .await
    }

    pub async fn delete_json(&self, path: &str) -> TeamsPipelineResult<Value> {
        self.request_json(Method::DELETE, path, &[], None).await
    }

    pub async fn collect_paginated(&self, path: &str) -> TeamsPipelineResult<Vec<Value>> {
        let mut next = Some(path.to_string());
        let mut values = Vec::new();
        while let Some(url) = next {
            let payload = self.get_json(&url, &[]).await?;
            if let Some(items) = payload.get("value").and_then(Value::as_array) {
                values.extend(items.iter().cloned());
            }
            next = payload
                .get("@odata.nextLink")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        Ok(values)
    }

    pub async fn download_bytes(&self, path_or_url: &str) -> TeamsPipelineResult<Vec<u8>> {
        let url = self.build_url(path_or_url, &[])?;
        let token = self.token_provider.get_access_token(false).await?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph download failed: {e}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(friendly_graph_error(status, &text));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph download read failed: {e}"),
            })?
            .to_vec())
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
    ) -> TeamsPipelineResult<Value> {
        let url = self.build_url(path, params)?;
        let token = self.token_provider.get_access_token(false).await?;
        let mut request = self.client.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph request failed: {e}"),
            })?;
        read_graph_json_response(response).await
    }

    fn build_url(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<reqwest::Url> {
        let mut url = if path.starts_with("http://") || path.starts_with("https://") {
            reqwest::Url::parse(path)
        } else {
            let base = self.base_url.trim_end_matches('/');
            let path = path.trim_start_matches('/');
            reqwest::Url::parse(&format!("{base}/{path}"))
        }
        .map_err(|e| TeamsPipelineError::Config(format!("invalid Graph URL: {e}")))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
        }
        Ok(url)
    }
}

fn graph_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap_or_else(|_| Client::new())
}

async fn read_graph_json_response(response: reqwest::Response) -> TeamsPipelineResult<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(friendly_graph_error(status, &text));
    }
    if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
        return Ok(json!({"success": true, "status_code": status.as_u16(), "empty": true}));
    }
    serde_json::from_str(&text).map_err(TeamsPipelineError::from)
}

fn friendly_graph_error(status: StatusCode, text: &str) -> TeamsPipelineError {
    let detail = extract_graph_error_message(text);
    let message = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            format!("Microsoft Graph permission denied or token expired: {detail}")
        }
        StatusCode::NOT_FOUND => format!("Microsoft Graph resource not found: {detail}"),
        StatusCode::TOO_MANY_REQUESTS => format!("Microsoft Graph rate limited: {detail}"),
        _ => detail,
    };
    TeamsPipelineError::Graph {
        status: status.as_u16(),
        message,
    }
}

fn extract_graph_error_message(text: &str) -> String {
    let fallback = text.trim().to_string();
    let Ok(payload) = serde_json::from_str::<Value>(text) else {
        return fallback;
    };
    payload
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .unwrap_or(&fallback)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct MicrosoftGraphTeamsBackend {
    client: MicrosoftGraphClient,
}

impl MicrosoftGraphTeamsBackend {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        Ok(Self {
            client: MicrosoftGraphClient::from_env()?,
        })
    }

    pub fn new(client: MicrosoftGraphClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &MicrosoftGraphClient {
        &self.client
    }
}

#[async_trait]
impl TeamsGraphBackend for MicrosoftGraphTeamsBackend {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        if let Some(meeting_id) = meeting_id.filter(|value| !value.trim().is_empty()) {
            let payload = self.client.get_json(&meeting_path(meeting_id), &[]).await?;
            return normalize_meeting_ref(&payload, tenant_id);
        }
        if let Some(join_web_url) = join_web_url.filter(|value| !value.trim().is_empty()) {
            let escaped = join_web_url.replace('\'', "''");
            let payload = self
                .client
                .get_json(
                    "/communications/onlineMeetings",
                    &[("$filter", format!("JoinWebUrl eq '{escaped}'"))],
                )
                .await?;
            let candidate = payload
                .get("value")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .ok_or_else(|| {
                    TeamsPipelineError::Invalid(format!(
                        "Teams meeting not found for join URL: {join_web_url}"
                    ))
                })?;
            return normalize_meeting_ref(candidate, tenant_id);
        }
        Err(TeamsPipelineError::Invalid(
            "Either meeting_id or join_web_url is required.".into(),
        ))
    }

    async fn fetch_preferred_transcript_text(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        let transcripts = self
            .client
            .collect_paginated(&format!(
                "{}/transcripts",
                meeting_path(&meeting_ref.meeting_id)
            ))
            .await?
            .into_iter()
            .filter_map(|payload| normalize_artifact("transcript", &payload, None).ok())
            .collect::<Vec<_>>();
        let Some(transcript) = select_preferred_transcript(&transcripts) else {
            return Ok(None);
        };
        let path = transcript_download_path(meeting_ref, &transcript);
        let bytes = self.client.download_bytes(&path).await?;
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some((transcript, text)))
    }

    async fn list_recording_artifacts(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        let payloads = self
            .client
            .collect_paginated(&format!(
                "{}/recordings",
                meeting_path(&meeting_ref.meeting_id)
            ))
            .await?;
        Ok(payloads
            .into_iter()
            .filter_map(|payload| normalize_artifact("recording", &payload, None).ok())
            .collect())
    }

    async fn download_recording_artifact(
        &self,
        meeting_ref: &TeamsMeetingRef,
        recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        self.client
            .download_bytes(&recording_download_path(meeting_ref, recording))
            .await
    }

    async fn enrich_meeting_with_call_record(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        let Some(call_record_id) = call_record_id.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        let path = format!(
            "/communications/callRecords/{}",
            path_percent_encode(call_record_id)
        );
        match self.client.get_json(&path, &[]).await {
            Ok(payload) => Ok(fetch_call_record_artifact_from_payload(&payload)),
            Err(TeamsPipelineError::Graph {
                status: 401 | 403 | 404,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn meeting_path(meeting_id: &str) -> String {
    format!(
        "/communications/onlineMeetings/{}",
        path_percent_encode(meeting_id)
    )
}

fn recording_download_path(meeting_ref: &TeamsMeetingRef, artifact: &MeetingArtifact) -> String {
    artifact.download_url.clone().unwrap_or_else(|| {
        format!(
            "{}/recordings/{}/content",
            meeting_path(&meeting_ref.meeting_id),
            path_percent_encode(&artifact.artifact_id)
        )
    })
}

fn transcript_download_path(meeting_ref: &TeamsMeetingRef, artifact: &MeetingArtifact) -> String {
    artifact.download_url.clone().unwrap_or_else(|| {
        format!(
            "{}/transcripts/{}/content",
            meeting_path(&meeting_ref.meeting_id),
            path_percent_encode(&artifact.artifact_id)
        )
    })
}

fn normalize_meeting_ref(
    payload: &Value,
    tenant_id: Option<&str>,
) -> TeamsPipelineResult<TeamsMeetingRef> {
    let object = payload.as_object().ok_or_else(|| {
        TeamsPipelineError::Invalid("Graph meeting payload was not an object.".into())
    })?;
    let mut metadata = Map::new();
    for key in [
        "subject",
        "startDateTime",
        "endDateTime",
        "createdDateTime",
        "participants",
    ] {
        if let Some(value) = object.get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    let meeting_ref = TeamsMeetingRef {
        meeting_id: object
            .get("id")
            .and_then(value_to_string)
            .unwrap_or_default(),
        organizer_user_id: parse_organizer_user_id(payload),
        join_web_url: object.get("joinWebUrl").and_then(value_to_string),
        calendar_event_id: object.get("calendarEventId").and_then(value_to_string),
        thread_id: parse_thread_id(payload),
        tenant_id: tenant_id
            .map(ToOwned::to_owned)
            .or_else(|| object.get("tenantId").and_then(value_to_string)),
        metadata,
    };
    meeting_ref.validate()?;
    Ok(meeting_ref)
}

fn parse_organizer_user_id(payload: &Value) -> Option<String> {
    payload
        .get("organizer")
        .and_then(|v| v.get("identity"))
        .and_then(|v| v.get("user"))
        .and_then(|v| v.get("id"))
        .and_then(value_to_string)
}

fn parse_thread_id(payload: &Value) -> Option<String> {
    payload
        .get("chatInfo")
        .and_then(|v| v.get("threadId"))
        .and_then(value_to_string)
        .or_else(|| payload.get("threadId").and_then(value_to_string))
}

fn normalize_artifact(
    artifact_type: &str,
    payload: &Value,
    default_source_url: Option<String>,
) -> TeamsPipelineResult<MeetingArtifact> {
    let object = payload.as_object().cloned().unwrap_or_default();
    let download_url = object
        .get("@microsoft.graph.downloadUrl")
        .and_then(value_to_string)
        .or_else(|| object.get("downloadUrl").and_then(value_to_string))
        .or_else(|| object.get("recordingContentUrl").and_then(value_to_string))
        .or_else(|| object.get("transcriptContentUrl").and_then(value_to_string));
    let source_url = object
        .get("webUrl")
        .and_then(value_to_string)
        .or_else(|| object.get("contentUrl").and_then(value_to_string))
        .or(default_source_url);
    let artifact = MeetingArtifact {
        artifact_type: artifact_type.into(),
        artifact_id: object
            .get("id")
            .and_then(value_to_string)
            .unwrap_or_default(),
        display_name: object
            .get("displayName")
            .and_then(value_to_string)
            .or_else(|| object.get("name").and_then(value_to_string)),
        content_type: object
            .get("contentType")
            .and_then(value_to_string)
            .or_else(|| object.get("fileMimeType").and_then(value_to_string)),
        source_url,
        download_url,
        created_at: object.get("createdDateTime").and_then(value_to_string),
        available_at: object
            .get("lastModifiedDateTime")
            .and_then(value_to_string)
            .or_else(|| object.get("meetingEndDateTime").and_then(value_to_string)),
        size_bytes: object.get("size").and_then(Value::as_u64),
        metadata: object,
    };
    artifact.validate()?;
    Ok(artifact)
}

pub fn select_preferred_transcript(candidates: &[MeetingArtifact]) -> Option<MeetingArtifact> {
    let mut transcripts = candidates
        .iter()
        .filter(|candidate| candidate.artifact_type == "transcript")
        .cloned()
        .collect::<Vec<_>>();
    transcripts.sort_by_key(transcript_sort_key);
    transcripts.pop()
}

fn transcript_sort_key(artifact: &MeetingArtifact) -> (u8, u8, String) {
    let status = artifact
        .metadata
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_completed = u8::from(matches!(
        status.as_str(),
        "available" | "completed" | "succeeded"
    ));
    let has_download = u8::from(artifact.download_url.is_some() || artifact.source_url.is_some());
    let timestamp = artifact
        .available_at
        .clone()
        .or_else(|| artifact.created_at.clone())
        .unwrap_or_default();
    (is_completed, has_download, timestamp)
}

fn fetch_call_record_artifact_from_payload(payload: &Value) -> Option<MeetingArtifact> {
    let object = payload.as_object()?;
    let id = object.get("id").and_then(value_to_string)?;
    let sessions = object
        .get("sessions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let participants = object
        .get("participants")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut metrics = Map::new();
    if let Some(version) = object.get("version") {
        metrics.insert("version".into(), version.clone());
    }
    if let Some(modalities) = object.get("modalities") {
        metrics.insert("modalities".into(), modalities.clone());
    }
    metrics.insert("participant_count".into(), json!(participants));
    if sessions > 0 {
        metrics.insert("session_count".into(), json!(sessions));
    }
    if let Some(organizer) = parse_organizer_user_id(payload) {
        metrics.insert("organizer".into(), Value::String(organizer));
    }
    let mut metadata = Map::new();
    metadata.insert("call_record".into(), payload.clone());
    metadata.insert("metrics".into(), Value::Object(metrics));
    Some(MeetingArtifact {
        artifact_type: "call_record".into(),
        artifact_id: id,
        display_name: object
            .get("type")
            .and_then(value_to_string)
            .or_else(|| Some("call_record".into())),
        content_type: None,
        source_url: object.get("webUrl").and_then(value_to_string),
        download_url: None,
        created_at: object.get("startDateTime").and_then(value_to_string),
        available_at: object.get("endDateTime").and_then(value_to_string),
        size_bytes: None,
        metadata,
    })
}
