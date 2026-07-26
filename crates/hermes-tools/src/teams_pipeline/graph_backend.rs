const MAX_GRAPH_DOWNLOAD_REDIRECTS: usize = 3;
const TRUSTED_PREAUTH_DOWNLOAD_HOST_SUFFIXES: &[&str] = &[
    "sharepoint.com",
    "sharepoint-df.com",
    "onedrive.com",
    "1drv.com",
    "blob.core.windows.net",
];

struct GraphDownloadTarget {
    url: reqwest::Url,
    attach_bearer: bool,
}

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
        let body =
            read_bounded_graph_response(response, MAX_TEAMS_GRAPH_TOKEN_BYTES, "token").await?;
        let text = String::from_utf8_lossy(&body);
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
        Ok(self.get_json_with_size(path, params).await?.0)
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
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        let mut page_count = 0_usize;
        let mut total_bytes = 0_usize;
        while let Some(next_link) = next {
            if page_count >= MAX_TEAMS_GRAPH_PAGINATION_PAGES {
                return Err(TeamsPipelineError::Retryable(format!(
                    "Graph pagination exceeded the {MAX_TEAMS_GRAPH_PAGINATION_PAGES} page limit."
                )));
            }
            let canonical_url = self.build_url(&next_link, &[])?.to_string();
            if !seen.insert(canonical_url.clone()) {
                return Err(TeamsPipelineError::Retryable(
                    "Graph pagination repeated an @odata.nextLink.".into(),
                ));
            }
            let (payload, body_bytes) = self.get_json_with_size(&canonical_url, &[]).await?;
            page_count += 1;
            total_bytes = total_bytes.checked_add(body_bytes).ok_or_else(|| {
                TeamsPipelineError::Retryable("Graph pagination response size overflowed.".into())
            })?;
            if total_bytes > MAX_TEAMS_GRAPH_PAGINATION_BYTES {
                return Err(TeamsPipelineError::Retryable(format!(
                    "Graph pagination exceeded the {MAX_TEAMS_GRAPH_PAGINATION_BYTES} byte limit."
                )));
            }
            if let Some(items) = payload.get("value").and_then(Value::as_array) {
                let item_count = values.len().checked_add(items.len()).ok_or_else(|| {
                    TeamsPipelineError::Retryable("Graph pagination item count overflowed.".into())
                })?;
                if item_count > MAX_TEAMS_GRAPH_PAGINATION_ITEMS {
                    return Err(TeamsPipelineError::Retryable(format!(
                        "Graph pagination exceeded the {MAX_TEAMS_GRAPH_PAGINATION_ITEMS} item limit."
                    )));
                }
                values.extend(items.iter().cloned());
            }
            next = payload
                .get("@odata.nextLink")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        Ok(values)
    }

    async fn download_bytes(
        &self,
        path_or_url: &str,
        max_bytes: usize,
        artifact_kind: &str,
    ) -> TeamsPipelineResult<Vec<u8>> {
        let mut target = self.build_download_target(path_or_url)?;
        let mut bearer_token = None;
        for redirect_count in 0..=MAX_GRAPH_DOWNLOAD_REDIRECTS {
            let mut request = self.client.get(target.url.clone());
            if target.attach_bearer {
                let token = match bearer_token.as_ref() {
                    Some(token) => token,
                    None => {
                        bearer_token = Some(self.token_provider.get_access_token(false).await?);
                        bearer_token
                            .as_ref()
                            .expect("Graph bearer token was initialized")
                    }
                };
                request = request.bearer_auth(token);
            }
            let response = request.send().await.map_err(|error| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph {artifact_kind} download failed: {error}"),
                retry_after_secs: None,
            })?;
            let status = response.status();
            if status.is_redirection() {
                if redirect_count == MAX_GRAPH_DOWNLOAD_REDIRECTS {
                    return Err(TeamsPipelineError::Retryable(format!(
                        "Graph {artifact_kind} download exceeded the {MAX_GRAPH_DOWNLOAD_REDIRECTS} redirect limit."
                    )));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or_else(|| {
                        TeamsPipelineError::Retryable(format!(
                            "Graph {artifact_kind} download redirect omitted Location."
                        ))
                    })?
                    .to_str()
                    .map_err(|error| {
                        TeamsPipelineError::Retryable(format!(
                            "Graph {artifact_kind} download redirect had an invalid Location: {error}"
                        ))
                    })?;
                let next_url = target.url.join(location).map_err(|error| {
                    TeamsPipelineError::Retryable(format!(
                        "Graph {artifact_kind} download redirect was invalid: {error}"
                    ))
                })?;
                let next_target = self.classify_download_url(next_url)?;
                if !target.attach_bearer && next_target.attach_bearer {
                    return Err(TeamsPipelineError::Retryable(format!(
                        "pre-authenticated {artifact_kind} download may not redirect into the Graph authorization origin."
                    )));
                }
                target = next_target;
                continue;
            }
            if !status.is_success() {
                let retry_after_secs = graph_retry_after_secs(response.headers());
                let body =
                    read_bounded_graph_response(response, MAX_TEAMS_GRAPH_ERROR_BYTES, "error")
                        .await?;
                return Err(friendly_graph_error(
                    status,
                    &String::from_utf8_lossy(&body),
                    retry_after_secs,
                ));
            }
            return read_bounded_graph_response(response, max_bytes, artifact_kind).await;
        }
        Err(TeamsPipelineError::Retryable(format!(
            "Graph {artifact_kind} download redirect handling ended unexpectedly."
        )))
    }

    async fn get_json_with_size(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<(Value, usize)> {
        self.request_json_with_size(Method::GET, path, params, None)
            .await
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
    ) -> TeamsPipelineResult<Value> {
        Ok(self
            .request_json_with_size(method, path, params, body)
            .await?
            .0)
    }

    async fn request_json_with_size(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
    ) -> TeamsPipelineResult<(Value, usize)> {
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
                retry_after_secs: None,
            })?;
        read_graph_json_response(response).await
    }

    fn graph_base_url(&self) -> TeamsPipelineResult<reqwest::Url> {
        reqwest::Url::parse(self.base_url.trim_end_matches('/'))
            .map_err(|error| TeamsPipelineError::Config(format!("invalid Graph base URL: {error}")))
    }

    fn resolve_url(&self, path: &str) -> TeamsPipelineResult<reqwest::Url> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return reqwest::Url::parse(path)
                .map_err(|error| TeamsPipelineError::Config(format!("invalid Graph URL: {error}")));
        }
        let base = self.base_url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        reqwest::Url::parse(&format!("{base}/{path}"))
            .map_err(|error| TeamsPipelineError::Config(format!("invalid Graph URL: {error}")))
    }

    fn build_url(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<reqwest::Url> {
        let mut url = self.resolve_url(path)?;
        let graph_base = self.graph_base_url()?;
        if !same_url_origin(&url, &graph_base) {
            return Err(TeamsPipelineError::Invalid(
                "refusing an absolute Graph URL outside the configured Graph origin.".into(),
            ));
        }
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
        }
        Ok(url)
    }

    fn build_download_target(&self, path_or_url: &str) -> TeamsPipelineResult<GraphDownloadTarget> {
        self.classify_download_url(self.resolve_url(path_or_url)?)
    }

    fn classify_download_url(
        &self,
        url: reqwest::Url,
    ) -> TeamsPipelineResult<GraphDownloadTarget> {
        if same_url_origin(&url, &self.graph_base_url()?) {
            return Ok(GraphDownloadTarget {
                url,
                attach_bearer: true,
            });
        }
        if is_trusted_pre_authenticated_download_url(&url) {
            return Ok(GraphDownloadTarget {
                url,
                attach_bearer: false,
            });
        }
        Err(TeamsPipelineError::Invalid(
            "refusing an untrusted external recording or transcript download URL.".into(),
        ))
    }
}

fn same_url_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_trusted_pre_authenticated_download_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.query().is_none_or(str::is_empty)
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    TRUSTED_PREAUTH_DOWNLOAD_HOST_SUFFIXES.iter().any(|suffix| {
        host == *suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn graph_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("static Graph HTTP client configuration is valid")
}

async fn read_bounded_graph_response(
    mut response: reqwest::Response,
    max_bytes: usize,
    response_kind: &str,
) -> TeamsPipelineResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(TeamsPipelineError::Retryable(format!(
            "Graph {response_kind} response exceeds the {max_bytes} byte limit."
        )));
    }
    let initial_capacity = response
        .content_length()
        .unwrap_or_default()
        .min(max_bytes as u64)
        .min(64 * 1024) as usize;
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| TeamsPipelineError::Graph {
        status: 0,
        message: format!("Graph {response_kind} download read failed: {error}"),
        retry_after_secs: None,
    })? {
        let total = body.len().checked_add(chunk.len()).ok_or_else(|| {
            TeamsPipelineError::Retryable(format!(
                "Graph {response_kind} response size overflowed its limit."
            ))
        })?;
        if total > max_bytes {
            return Err(TeamsPipelineError::Retryable(format!(
                "Graph {response_kind} response exceeds the {max_bytes} byte limit."
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
async fn read_graph_json_response(
    response: reqwest::Response,
) -> TeamsPipelineResult<(Value, usize)> {
    let status = response.status();
    let retry_after_secs = graph_retry_after_secs(response.headers());
    let max_bytes = if status.is_success() {
        MAX_TEAMS_GRAPH_JSON_BYTES
    } else {
        MAX_TEAMS_GRAPH_ERROR_BYTES
    };
    let body = read_bounded_graph_response(response, max_bytes, "JSON").await?;
    let body_bytes = body.len();
    let text = String::from_utf8_lossy(&body);
    if !status.is_success() {
        return Err(friendly_graph_error(status, &text, retry_after_secs));
    }
    if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
        return Ok((
            json!({"success": true, "status_code": status.as_u16(), "empty": true}),
            body_bytes,
        ));
    }
    serde_json::from_str(&text)
        .map(|value| (value, body_bytes))
        .map_err(TeamsPipelineError::from)
}

fn friendly_graph_error(
    status: StatusCode,
    text: &str,
    retry_after_secs: Option<u64>,
) -> TeamsPipelineError {
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
        retry_after_secs,
    }
}

fn graph_retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&Utc);
    let seconds = retry_at.signed_duration_since(Utc::now()).num_seconds();
    Some(seconds.max(0) as u64)
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
        organizer_user_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        let organizer_user_id = organizer_user_id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                TeamsPipelineError::Invalid(
                    "Teams user-scoped Graph requests require organizer_user_id.".into(),
                )
            })
            .and_then(normalize_organizer_user_id)?;
        if let Some(meeting_id) = meeting_id.filter(|value| !value.trim().is_empty()) {
            let payload = self
                .client
                .get_json(&meeting_path(&organizer_user_id, meeting_id)?, &[])
                .await?;
            return normalize_meeting_ref(
                &payload,
                tenant_id,
                &organizer_user_id,
                Some(meeting_id),
            );
        }
        if let Some(join_web_url) = join_web_url.filter(|value| !value.trim().is_empty()) {
            let escaped = join_web_url.replace('\'', "''");
            let payload = self
                .client
                .get_json(
                    &format!(
                        "/users/{}/onlineMeetings",
                        path_percent_encode(&organizer_user_id)
                    ),
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
            return normalize_meeting_ref(candidate, tenant_id, &organizer_user_id, None);
        }
        Err(TeamsPipelineError::Invalid(
            "Either meeting_id or join_web_url is required.".into(),
        ))
    }

    async fn fetch_preferred_transcript_text(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        let meeting_path = scoped_meeting_path(meeting_ref)?;
        let transcripts = self
            .client
            .collect_paginated(&format!("{meeting_path}/transcripts"))
            .await?
            .into_iter()
            .filter_map(|payload| normalize_artifact("transcript", &payload, None).ok())
            .collect::<Vec<_>>();
        let Some(transcript) = select_preferred_transcript(&transcripts) else {
            return Ok(None);
        };
        let path = transcript_download_path(meeting_ref, &transcript)?;
        let bytes = self
            .client
            .download_bytes(&path, MAX_TEAMS_TRANSCRIPT_BYTES, "transcript")
            .await?;
        let text = normalize_transcript_content(&transcript, &bytes)?;
        Ok(Some((transcript, text)))
    }


    async fn list_recording_artifacts(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        let meeting_path = scoped_meeting_path(meeting_ref)?;
        let payloads = self
            .client
            .collect_paginated(&format!("{meeting_path}/recordings"))
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
        let path = recording_download_path(meeting_ref, recording)?;
        self.client
            .download_bytes(&path, MAX_TEAMS_RECORDING_BYTES, "recording")
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

/// Graph transcript downloads are WebVTT unless metadata explicitly classifies
/// them as plain text. Do not pass captions, timestamps, or markup through to
/// the summarizer.
fn normalize_transcript_content(
    transcript: &MeetingArtifact,
    bytes: &[u8],
) -> TeamsPipelineResult<String> {
    let content_type = transcript
        .content_type
        .as_deref()
        .unwrap_or("text/vtt")
        .split(';')
        .next()
        .unwrap_or("text/vtt")
        .trim()
        .to_ascii_lowercase();
    match content_type.as_str() {
        "text/vtt" => normalize_webvtt_transcript(bytes),
        "text/plain" => normalize_plain_text_transcript(bytes),
        other => Err(TeamsPipelineError::Invalid(format!(
            "Unsupported Teams transcript content type {other:?}; only text/vtt or explicitly classified text/plain is accepted."
        ))),
    }
}

fn normalize_plain_text_transcript(bytes: &[u8]) -> TeamsPipelineResult<String> {
    let text = transcript_utf8(bytes)?;
    let text = collapse_transcript_whitespace(&decode_safe_vtt_entities(
        text.strip_prefix('\u{feff}').unwrap_or(text),
    ));
    if text.is_empty() {
        return Err(TeamsPipelineError::Invalid(
            "Teams plain-text transcript has no text.".into(),
        ));
    }
    Ok(text)
}

fn normalize_webvtt_transcript(bytes: &[u8]) -> TeamsPipelineResult<String> {
    let source = transcript_utf8(bytes)?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.lines();
    let header = lines
        .next()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).trim())
        .ok_or_else(|| TeamsPipelineError::Invalid("Teams VTT transcript is empty.".into()))?;
    if !is_webvtt_header(header) {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript is missing its WEBVTT header.".into(),
        ));
    }

    let mut header_complete = false;
    let mut block = Vec::new();
    let mut cues = Vec::new();
    for raw_line in lines {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if !header_complete {
            if line.trim().is_empty() {
                header_complete = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            normalize_webvtt_block(&block, &mut cues)?;
            block.clear();
        } else {
            block.push(line);
        }
    }
    if !header_complete {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript header is not terminated by a blank line.".into(),
        ));
    }
    normalize_webvtt_block(&block, &mut cues)?;
    if cues.is_empty() {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript contains no text cues.".into(),
        ));
    }
    Ok(cues.join("\n"))
}

fn transcript_utf8(bytes: &[u8]) -> TeamsPipelineResult<&str> {
    std::str::from_utf8(bytes).map_err(|_| {
        TeamsPipelineError::Invalid("Teams transcript must be valid UTF-8.".into())
    })
}

fn is_webvtt_header(line: &str) -> bool {
    line == "WEBVTT"
        || line
            .strip_prefix("WEBVTT")
            .is_some_and(|suffix| suffix.starts_with(char::is_whitespace))
}

fn normalize_webvtt_block(
    block: &[&str],
    cues: &mut Vec<String>,
) -> TeamsPipelineResult<()> {
    let Some(first) = block.first().map(|line| line.trim()) else {
        return Ok(());
    };
    if is_webvtt_metadata_block(first) {
        return Ok(());
    }
    let timing_index = usize::from(!first.contains("-->"));
    let timing = block.get(timing_index).map(|line| line.trim()).ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Teams VTT transcript cue is missing its timestamp line.".into(),
        )
    })?;
    validate_webvtt_timing(timing)?;
    let payload = block.get(timing_index + 1..).unwrap_or_default().join("\n");
    let cue = normalize_webvtt_cue_payload(&payload)?;
    cues.push(cue);
    Ok(())
}

fn is_webvtt_metadata_block(line: &str) -> bool {
    ["NOTE", "STYLE", "REGION"].into_iter().any(|keyword| {
        line.eq_ignore_ascii_case(keyword)
            || line
                .get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                && line
                    .get(keyword.len()..)
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(char::is_whitespace)
    })
}

fn validate_webvtt_timing(line: &str) -> TeamsPipelineResult<()> {
    let (start, end_and_settings) = line.split_once("-->").ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Teams VTT transcript cue is missing the --> timestamp separator.".into(),
        )
    })?;
    if end_and_settings.contains("-->") {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript cue has multiple timestamp separators.".into(),
        ));
    }
    let end = end_and_settings
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default();
    let start = parse_webvtt_timestamp(start.trim()).ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Teams VTT transcript cue has an invalid start timestamp.".into(),
        )
    })?;
    let end = parse_webvtt_timestamp(end).ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Teams VTT transcript cue has an invalid end timestamp.".into(),
        )
    })?;
    if end < start {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript cue ends before it starts.".into(),
        ));
    }
    Ok(())
}

fn parse_webvtt_timestamp(value: &str) -> Option<u64> {
    let mut parts = value.split(':');
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (hours, minutes, seconds) = match third {
        Some(seconds) => (
            parse_ascii_u64(first)?,
            parse_ascii_u64(second).filter(|minutes| *minutes < 60)?,
            seconds,
        ),
        None => (0, parse_ascii_u64(first)?, second),
    };
    let (seconds, milliseconds) = seconds.split_once('.')?;
    if milliseconds.contains('.') || milliseconds.len() != 3 {
        return None;
    }
    let seconds = parse_ascii_u64(seconds).filter(|seconds| *seconds < 60)?;
    let milliseconds = parse_ascii_u64(milliseconds)?;
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(milliseconds)
}

fn parse_ascii_u64(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn normalize_webvtt_cue_payload(payload: &str) -> TeamsPipelineResult<String> {
    let (speaker, payload) = strip_webvtt_tags(payload)?;
    let text = collapse_transcript_whitespace(&decode_safe_vtt_entities(&payload));
    if text.is_empty() {
        return Err(TeamsPipelineError::Invalid(
            "Teams VTT transcript cue has no text.".into(),
        ));
    }
    let speaker = speaker
        .as_deref()
        .map(decode_safe_vtt_entities)
        .map(|value| collapse_transcript_whitespace(&value))
        .filter(|value| !value.is_empty());
    Ok(match speaker {
        Some(speaker) => format!("{speaker}: {text}"),
        None => text,
    })
}

fn strip_webvtt_tags(input: &str) -> TeamsPipelineResult<(Option<String>, String)> {
    let mut speaker = None;
    let mut visible = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(open) = remaining.find('<') {
        visible.push_str(&remaining[..open]);
        remaining = &remaining[open + 1..];
        let close = remaining.find('>').ok_or_else(|| {
            TeamsPipelineError::Invalid(
                "Teams VTT transcript cue contains an unclosed markup tag.".into(),
            )
        })?;
        let tag = remaining[..close].trim();
        if speaker.is_none() {
            speaker = webvtt_voice_name(tag).map(ToOwned::to_owned);
        }
        remaining = &remaining[close + 1..];
    }
    visible.push_str(remaining);
    Ok((speaker, visible))
}

fn webvtt_voice_name(tag: &str) -> Option<&str> {
    let mut characters = tag.chars();
    let first = characters.next()?;
    if !first.eq_ignore_ascii_case(&'v') {
        return None;
    }
    let remainder = characters.as_str();
    remainder
        .starts_with(char::is_whitespace)
        .then_some(remainder.trim())
        .filter(|name| !name.is_empty())
}

fn collapse_transcript_whitespace(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    for word in input.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn decode_safe_vtt_entities(input: &str) -> String {
    let mut decoded = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(start) = remaining.find('&') {
        decoded.push_str(&remaining[..start]);
        let entity_and_rest = &remaining[start + 1..];
        let Some(end) = entity_and_rest.find(';') else {
            decoded.push('&');
            remaining = entity_and_rest;
            continue;
        };
        let entity = &entity_and_rest[..end];
        if entity.len() <= 16 {
            if let Some(character) = decode_safe_vtt_entity(entity) {
                decoded.push(character);
                remaining = &entity_and_rest[end + 1..];
                continue;
            }
        }
        decoded.push('&');
        remaining = entity_and_rest;
    }
    decoded.push_str(remaining);
    decoded
}

fn decode_safe_vtt_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let numeric = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X"));
            let value = numeric
                .and_then(|value| u32::from_str_radix(value, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|value| value.parse::<u32>().ok())
                })?;
            char::from_u32(value).filter(|character| !character.is_control())
        }
    }
}

fn meeting_path(organizer_user_id: &str, meeting_id: &str) -> TeamsPipelineResult<String> {
    let organizer_user_id = normalize_organizer_user_id(organizer_user_id)?;
    let meeting_id = meeting_id.trim();
    if meeting_id.is_empty() {
        return Err(TeamsPipelineError::Invalid(
            "Teams Graph request requires a meeting_id.".into(),
        ));
    }
    Ok(format!(
        "/users/{}/onlineMeetings/{}",
        path_percent_encode(&organizer_user_id),
        path_percent_encode(meeting_id)
    ))
}

fn scoped_meeting_path(meeting_ref: &TeamsMeetingRef) -> TeamsPipelineResult<String> {
    let organizer_user_id = meeting_ref.organizer_user_id.as_deref().ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Teams Graph artifact requests require organizer_user_id.".into(),
        )
    })?;
    meeting_path(organizer_user_id, &meeting_ref.meeting_id)
}

fn recording_download_path(
    meeting_ref: &TeamsMeetingRef,
    artifact: &MeetingArtifact,
) -> TeamsPipelineResult<String> {
    Ok(format!(
        "{}/recordings/{}/content",
        scoped_meeting_path(meeting_ref)?,
        path_percent_encode(&artifact.artifact_id)
    ))
}

fn transcript_download_path(
    meeting_ref: &TeamsMeetingRef,
    artifact: &MeetingArtifact,
) -> TeamsPipelineResult<String> {
    Ok(format!(
        "{}/transcripts/{}/content",
        scoped_meeting_path(meeting_ref)?,
        path_percent_encode(&artifact.artifact_id)
    ))
}

fn normalize_meeting_ref(
    payload: &Value,
    tenant_id: Option<&str>,
    organizer_user_id: &str,
    expected_meeting_id: Option<&str>,
) -> TeamsPipelineResult<TeamsMeetingRef> {
    let object = payload.as_object().ok_or_else(|| {
        TeamsPipelineError::Invalid("Graph meeting payload was not an object.".into())
    })?;
    let payload_meeting_id = object
        .get("id")
        .and_then(value_to_string)
        .ok_or_else(|| TeamsPipelineError::Invalid("Graph meeting payload omitted id.".into()))?;
    if let Some(expected_meeting_id) = expected_meeting_id {
        if payload_meeting_id != expected_meeting_id.trim() {
            return Err(TeamsPipelineError::Invalid(
                "Graph meeting payload id does not match the requested meeting id.".into(),
            ));
        }
    }
    let organizer_user_id = normalize_organizer_user_id(organizer_user_id)?;
    if let Some(payload_organizer) = parse_organizer_user_id(payload) {
        if normalize_organizer_user_id(&payload_organizer)? != organizer_user_id {
            return Err(TeamsPipelineError::Invalid(
                "Graph meeting payload organizer does not match the requested organizer.".into(),
            ));
        }
    }
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
        meeting_id: payload_meeting_id,
        organizer_user_id: Some(organizer_user_id),
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
