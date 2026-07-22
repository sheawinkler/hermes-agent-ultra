const DISCORD_REST_JSON_BODY_LIMIT_BYTES: usize = 1024 * 1024;
const DISCORD_REST_ERROR_BODY_LIMIT_BYTES: usize = 8 * 1024;
const DISCORD_REST_RATE_LIMIT_MAX_WAIT: std::time::Duration =
    // Gateway delivery owns a 30-second commit deadline. Keep this under that
    // boundary so a later 429 returns a durable PartialDelivery rather than
    // being cancelled by the outer timeout after an earlier chunk was accepted.
    std::time::Duration::from_secs(20);
/// Upper bound for a single attachment transfer. It remains below the
/// gateway's ten-minute adapter file-delivery deadline so the caller retains
/// time to classify and persist the result.
const DISCORD_UPLOAD_TRANSFER_MAX_WAIT: std::time::Duration =
    std::time::Duration::from_secs(9 * 60 + 50);
const DISCORD_LIVENESS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Maximum attachment size supported by the Discord bot upload surface.
///
/// Validate metadata before allocating or constructing multipart state, then
/// bound the actual read to protect against a file growing concurrently.
const DISCORD_UPLOAD_FILE_LIMIT_BYTES: u64 = 25 * 1024 * 1024;

fn discord_safe_attachment_file_name(file_path: &str) -> String {
    Path::new(file_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("attachment")
        .to_string()
}

fn discord_local_media_preflight_failure(
    file_path: &str,
    error: &std::io::Error,
) -> GatewayError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => MediaPreflightFailure::NotFound,
        _ => MediaPreflightFailure::Unreadable,
    };
    let file_name = discord_safe_attachment_file_name(file_path);
    warn!(
        media_file = %file_name,
        error = %error,
        error_code = %code,
        "Discord media preflight failed before an upload request"
    );
    GatewayError::MediaPreflightFailed { file_name, code }
}

fn discord_oversized_media_preflight_failure(file_path: &str, bytes: u64) -> GatewayError {
    let file_name = discord_safe_attachment_file_name(file_path);
    warn!(
        media_file = %file_name,
        bytes,
        limit = DISCORD_UPLOAD_FILE_LIMIT_BYTES,
        "Discord media preflight rejected an oversized attachment before an upload request"
    );
    GatewayError::MediaPreflightFailed {
        file_name,
        code: MediaPreflightFailure::TooLarge,
    }
}

/// Adapter-wide admission gate for Discord's `global: true` REST 429s.
///
/// Discord can invalidate every route for a bot account at once. Keeping the
/// deadline in a watch channel lets every independent outbound operation see
/// monotonically extended pauses without serializing normal requests.
#[derive(Debug)]
struct DiscordGlobalRateLimitBarrier {
    not_before: tokio::sync::watch::Sender<Option<tokio::time::Instant>>,
    #[cfg(test)]
    wait_started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl Default for DiscordGlobalRateLimitBarrier {
    fn default() -> Self {
        let (not_before, _) = tokio::sync::watch::channel(None);
        Self {
            not_before,
            #[cfg(test)]
            wait_started: std::sync::Mutex::new(None),
        }
    }
}

impl DiscordGlobalRateLimitBarrier {
    fn extend(&self, retry_after: std::time::Duration) {
        let Some(candidate) = tokio::time::Instant::now().checked_add(retry_after) else {
            return;
        };
        self.not_before.send_if_modified(|not_before| {
            if not_before.is_none_or(|current| current < candidate) {
                *not_before = Some(candidate);
                true
            } else {
                false
            }
        });
    }

    #[cfg(test)]
    fn observe_next_wait(&self) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *self
            .wait_started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        receiver
    }

    async fn wait_until_open(
        &self,
        deadline: tokio::time::Instant,
        stop: &mut tokio::sync::watch::Receiver<bool>,
        operation: &str,
    ) -> Result<(), GatewayError> {
        let mut updates = self.not_before.subscribe();
        loop {
            if *stop.borrow() {
                return Err(GatewayError::SendFailed(format!(
                    "{operation} cancelled because the Discord adapter is stopping"
                )));
            }
            let Some(not_before) = *updates.borrow_and_update() else {
                return Ok(());
            };
            if not_before <= tokio::time::Instant::now() {
                return Ok(());
            }
            #[cfg(test)]
            if let Some(sender) = self
                .wait_started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(());
            }

            tokio::select! {
                _ = tokio::time::sleep_until(not_before) => {}
                changed = updates.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(GatewayError::RateLimited {
                        retry_after: Some(
                            not_before
                                .saturating_duration_since(tokio::time::Instant::now()),
                        ),
                    });
                }
                changed = stop.changed() => {
                    let _ = changed;
                    return Err(GatewayError::SendFailed(format!(
                        "{operation} cancelled because the Discord adapter is stopping"
                    )));
                }
            }
        }
    }
}

/// A parent-channel fallback is safe only when Discord conclusively rejected
/// creation before it could create the stable message thread. Transport,
/// server, and recovery failures may leave that thread existing.
///
/// `Retryable` is reserved for conditions proven to occur before a POST was
/// accepted (for example a pre-existing global rate-limit barrier) or a 429
/// response. It must not be collapsed into `Ambiguous`, because the durable
/// ingress head can safely remain FIFO and retry it.
#[derive(Debug)]
enum DiscordThreadCreationFailure {
    Rejected(GatewayError),
    Retryable(GatewayError),
    Ambiguous(GatewayError),
}

/// Snapshot state committed by a supervised Discord tracker writer. The actor
/// owns and awaits each blocking atomic replacement, so a cancelled caller can
/// drop its acknowledgement without detaching or reordering the write.
trait DiscordTrackerSnapshot: Clone + Send + 'static {
    fn save_snapshot(&mut self) -> std::io::Result<()>;
}

impl DiscordTrackerSnapshot for DiscordThreadParticipationTracker {
    fn save_snapshot(&mut self) -> std::io::Result<()> {
        self.save_staged()
    }
}

impl DiscordTrackerSnapshot for DiscordNonConversationalMessageTracker {
    fn save_snapshot(&mut self) -> std::io::Result<()> {
        self.save_staged()
    }
}

type DiscordTrackerMutation<T> = Box<dyn FnOnce(&mut T) -> Option<T> + Send>;

enum DiscordTrackerWriteRequest<T: DiscordTrackerSnapshot> {
    Mutate {
        mutation: DiscordTrackerMutation<T>,
        acknowledgement: tokio::sync::oneshot::Sender<std::io::Result<bool>>,
    },
    Flush {
        acknowledgement: tokio::sync::oneshot::Sender<std::io::Result<bool>>,
    },
    Shutdown {
        acknowledgement: tokio::sync::oneshot::Sender<()>,
    },
}

/// Actor-owned durable snapshot pipeline for one Discord tracker.
///
/// `pending` is the exact staged snapshot whose prior fsync/replace failed.
/// It remains the base for every later mutation and is cleared only after that
/// exact state (or a superset containing it) is durably committed.
struct DiscordTrackerWriter<T: DiscordTrackerSnapshot> {
    sender: tokio::sync::mpsc::UnboundedSender<DiscordTrackerWriteRequest<T>>,
    task: tokio::task::JoinHandle<()>,
}

impl<T: DiscordTrackerSnapshot> DiscordTrackerWriter<T> {
    fn start(
        state: Arc<Mutex<T>>,
        repair_pending: Arc<AtomicBool>,
    ) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut pending: Option<T> = None;
            while let Some(request) = receiver.recv().await {
                match request {
                    DiscordTrackerWriteRequest::Mutate {
                        mutation,
                        acknowledgement,
                    } => {
                        let retrying = pending.is_some();
                        let mut base = match pending.take() {
                            Some(snapshot) => snapshot,
                            None => match state.lock() {
                                Ok(tracker) => tracker.clone(),
                                Err(_) => {
                                    let _ = acknowledgement.send(Err(std::io::Error::other(
                                        "discord tracker lock poisoned",
                                    )));
                                    continue;
                                }
                            },
                        };
                        let candidate = match mutation(&mut base) {
                            Some(candidate) => candidate,
                            None if retrying => base,
                            None => {
                                let _ = acknowledgement.send(Ok(false));
                                continue;
                            }
                        };
                        let result = Self::commit_candidate(
                            &state,
                            &repair_pending,
                            &mut pending,
                            candidate,
                        )
                        .await;
                        let _ = acknowledgement.send(result);
                    }
                    DiscordTrackerWriteRequest::Flush { acknowledgement } => {
                        let result = match pending.take() {
                            Some(candidate) => Self::commit_candidate(
                                &state,
                                &repair_pending,
                                &mut pending,
                                candidate,
                            )
                            .await,
                            None => Ok(false),
                        };
                        let _ = acknowledgement.send(result);
                    }
                    DiscordTrackerWriteRequest::Shutdown { acknowledgement } => {
                        let _ = acknowledgement.send(());
                        break;
                    }
                }
            }
        });
        Self { sender, task }
    }

    async fn commit_candidate(
        state: &Arc<Mutex<T>>,
        repair_pending: &Arc<AtomicBool>,
        pending: &mut Option<T>,
        candidate: T,
    ) -> std::io::Result<bool> {
        let (candidate, write_result) = tokio::task::spawn_blocking(move || {
            let mut candidate = candidate;
            let result = candidate.save_snapshot();
            (candidate, result)
        })
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "discord tracker persistence worker terminated: {error}"
            ))
        })?;
        match write_result {
            Ok(()) => match state.lock() {
                Ok(mut tracker) => {
                    *tracker = candidate;
                    repair_pending.store(false, Ordering::Release);
                    Ok(true)
                }
                Err(_) => {
                    *pending = Some(candidate);
                    repair_pending.store(true, Ordering::Release);
                    Err(std::io::Error::other("discord tracker lock poisoned"))
                }
            },
            Err(error) => {
                *pending = Some(candidate);
                repair_pending.store(true, Ordering::Release);
                Err(error)
            }
        }
    }

    fn sender(&self) -> tokio::sync::mpsc::UnboundedSender<DiscordTrackerWriteRequest<T>> {
        self.sender.clone()
    }

    async fn shutdown(self) -> std::io::Result<()> {
        let Self { sender, task } = self;
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(DiscordTrackerWriteRequest::Shutdown { acknowledgement })
            .map_err(|_| std::io::Error::other("discord tracker writer stopped"))?;
        drop(sender);
        receiver
            .await
            .map_err(|_| std::io::Error::other("discord tracker writer stopped"))?;
        task.await.map_err(|error| {
            std::io::Error::other(format!("discord tracker writer join failed: {error}"))
        })
    }

    fn abort(self) {
        self.task.abort();
    }
}

/// Admission fence for every Discord operation that can create an accepted
/// side effect followed by tracker persistence.
///
/// Closing the fence prevents a shutdown from racing a new REST side effect.
/// Permits already issued remain valid until their accompanying durable
/// tracker update has completed; shutdown waits for that set before it takes
/// either writer slot.
#[derive(Debug)]
struct DiscordAcceptedStateAdmission {
    state: Mutex<DiscordAcceptedStateAdmissionState>,
    in_flight_changed: tokio::sync::watch::Sender<usize>,
}

#[derive(Debug)]
struct DiscordAcceptedStateAdmissionState {
    accepting: bool,
    in_flight: usize,
}

impl Default for DiscordAcceptedStateAdmission {
    fn default() -> Self {
        let (in_flight_changed, _) = tokio::sync::watch::channel(0);
        Self {
            state: Mutex::new(DiscordAcceptedStateAdmissionState {
                accepting: true,
                in_flight: 0,
            }),
            in_flight_changed,
        }
    }
}

impl DiscordAcceptedStateAdmission {
    fn admit(self: &Arc<Self>) -> Result<DiscordAcceptedStatePermit, GatewayError> {
        let mut state = self.state.lock().map_err(|_| {
            GatewayError::Platform("Discord accepted-state admission lock poisoned".to_string())
        })?;
        if !state.accepting {
            return Err(GatewayError::SendFailed(
                "Discord adapter is stopping before accepted-state persistence admission".to_string(),
            ));
        }
        state.in_flight = state.in_flight.checked_add(1).ok_or_else(|| {
            GatewayError::Platform("Discord accepted-state admission overflow".to_string())
        })?;
        self.in_flight_changed.send_replace(state.in_flight);
        Ok(DiscordAcceptedStatePermit {
            admission: Arc::clone(self),
        })
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
    }

    fn reopen(&self) -> Result<(), GatewayError> {
        let mut state = self.state.lock().map_err(|_| {
            GatewayError::Platform("Discord accepted-state admission lock poisoned".to_string())
        })?;
        if state.in_flight != 0 {
            return Err(GatewayError::Platform(
                "Discord accepted-state commits remain in flight during startup".to_string(),
            ));
        }
        state.accepting = true;
        Ok(())
    }

    async fn wait_until_drained(&self) {
        let mut in_flight_changed = self.in_flight_changed.subscribe();
        while *in_flight_changed.borrow() != 0 {
            if in_flight_changed.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Keeps a pre-stop REST side effect and its required tracker commit in the
/// same shutdown generation.
struct DiscordAcceptedStatePermit {
    admission: Arc<DiscordAcceptedStateAdmission>,
}

impl Drop for DiscordAcceptedStatePermit {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.in_flight > 0);
        state.in_flight = state.in_flight.saturating_sub(1);
        self.admission
            .in_flight_changed
            .send_replace(state.in_flight);
    }
}

#[derive(Deserialize)]
struct DiscordGatewayBotResponse {
    #[serde(default)]
    session_start_limit: Option<DiscordGatewayBotSessionStartLimit>,
}

#[derive(Deserialize)]
struct DiscordGatewayBotSessionStartLimit {
    total: u64,
    remaining: u64,
    reset_after: u64,
}

#[derive(Deserialize)]
struct DiscordRateLimitResponse {
    #[serde(default)]
    global: bool,
    #[serde(default)]
    retry_after: Option<f64>,
}

/// Discord documents the JSON `retry_after` as the fractional source of
/// truth; HTTP `Retry-After` is only a fallback for incomplete bodies.
fn discord_rate_limit_delay(
    body: &str,
    header_retry_after: Option<std::time::Duration>,
) -> (std::time::Duration, bool) {
    let rate_limit = serde_json::from_str::<DiscordRateLimitResponse>(body).ok();
    let retry_after = rate_limit
        .as_ref()
        .and_then(|body| body.retry_after)
        .and_then(discord_retry_after_duration)
        .or(header_retry_after)
        .unwrap_or(std::time::Duration::from_secs(1));
    let global = matches!(rate_limit.as_ref(), Some(body) if body.global);
    (retry_after, global)
}

/// Discord may declare a global 429 in a header before the JSON body arrives.
/// Accept only the documented truthy values; unknown header values are not a
/// global admission barrier.
fn discord_rate_limit_is_global_header(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get("x-ratelimit-global")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1")
}

fn discord_bot_state_scope(config: &DiscordConfig) -> String {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;

    /// Canonicalize a private API/gateway realm for hashing only. This value
    /// is never logged or serialized: it may contain URL credentials and
    /// tenant query parameters.
    fn canonical_realm(raw: Option<&str>, fallback: &str, gateway: bool) -> String {
        let raw = raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback);
        let Ok(url) = url::Url::parse(raw) else {
            return raw.to_ascii_lowercase();
        };

        let mut query: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| {
                // The gateway driver owns these exact transport knobs. They
                // do not distinguish a private gateway tenant.
                !(gateway
                    && (key.eq_ignore_ascii_case("v")
                        || key.eq_ignore_ascii_case("encoding")
                        || key.eq_ignore_ascii_case("compress")))
            })
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        query.sort_unstable();
        let mut encoded_query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in query {
            encoded_query.append_pair(&key, &value);
        }
        let query = encoded_query.finish();

        // Length-delimit components so credentials and query values cannot
        // collide through separator characters. The full string remains local
        // to this function's SHA-256 input.
        fn append_component(output: &mut String, value: &str) {
            let _ = write!(output, "{}:", value.len());
            output.push_str(value);
        }

        let mut realm = String::new();
        let scheme = url.scheme().to_ascii_lowercase();
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let port = url.port_or_known_default().unwrap_or_default().to_string();
        append_component(&mut realm, &scheme);
        append_component(&mut realm, url.username());
        append_component(&mut realm, url.password().unwrap_or_default());
        append_component(&mut realm, &host);
        append_component(&mut realm, &port);
        append_component(&mut realm, url.path().trim_end_matches('/'));
        append_component(&mut realm, &query);
        realm
    }

    let application_id = config
        .application_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()));
    let token_user_id = config
        .token
        .trim()
        .split('.')
        .next()
        .and_then(|segment| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(segment)
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()));
    let identity = format!(
        "api={}\ngateway={}\napplication={}\nbot={}",
        canonical_realm(config.api_base_url.as_deref(), DISCORD_API_BASE, false),
        canonical_realm(config.gateway_url.as_deref(), DISCORD_GATEWAY_V10_URL, true),
        application_id.unwrap_or("none"),
        token_user_id.as_deref().unwrap_or("unknown"),
    );
    let mut digest = Sha256::new();
    digest.update(identity.as_bytes());
    // A normal Discord bot token exposes a stable bot ID in its first segment,
    // and an application ID is separately configured. Only opaque/private
    // gateway tokens need a secret-derived discriminator.
    if application_id.is_none() && token_user_id.is_none() {
        digest.update(Sha256::digest(config.token.trim().as_bytes()));
    }
    let digest = digest.finalize();
    let mut scope = String::from("discord-");
    for byte in &digest[..16] {
        let _ = write!(scope, "{byte:02x}");
    }
    scope
}

/// Zero disables liveness probing. Any other value must be representable as a
/// `Duration`; rejecting invalid values during adapter construction prevents a
/// later probe-start panic after the socket has already been claimed.
fn validate_discord_liveness_interval(
    seconds: f64,
) -> Result<Option<std::time::Duration>, GatewayError> {
    if seconds == 0.0 {
        return Ok(None);
    }
    std::time::Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|interval| !interval.is_zero())
        .map(Some)
        .ok_or_else(|| {
            GatewayError::Platform(
                "Discord liveness_interval_seconds must be 0 or a positive representable duration"
                    .to_string(),
            )
        })
}


impl DiscordAdapter {
    /// Stable, non-secret identity namespace for persisted Discord state.
    pub fn state_scope_for_config(config: &DiscordConfig) -> String {
        discord_bot_state_scope(config)
    }

    /// Create a new Discord adapter with the default global state location.
    ///
    /// CLI callers that select a config/profile state root must use
    /// [`Self::new_with_state_root`] so trackers cannot cross profile bounds.
    pub fn new(config: DiscordConfig) -> Result<Self, GatewayError> {
        let state_scope = discord_bot_state_scope(&config);
        Self::new_with_trackers(
            config,
            DiscordThreadParticipationTracker::new(&state_scope),
            DiscordNonConversationalMessageTracker::new(&state_scope),
        )
    }

    /// Create a Discord adapter whose durable tracker files live under an
    /// explicit, already identity-scoped state directory.
    ///
    /// The caller owns the profile/identity namespace. Both tracker types use
    /// this same root rather than `hermes_home`, preventing one CLI profile
    /// from inheriting another bot's thread or message IDs.
    pub fn new_with_state_root(
        config: DiscordConfig,
        state_root: impl AsRef<std::path::Path>,
    ) -> Result<Self, GatewayError> {
        let state_root = state_root.as_ref();
        Self::new_with_trackers(
            config,
            DiscordThreadParticipationTracker::from_path(
                state_root.join("threads.json"),
                DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
            ),
            DiscordNonConversationalMessageTracker::from_path(
                state_root.join(DISCORD_NONCONVERSATIONAL_STATE_FILENAME),
                DiscordNonConversationalMessageTracker::DEFAULT_MAX_TRACKED,
            ),
        )
    }

    fn new_with_trackers(
        mut config: DiscordConfig,
        thread_participation: DiscordThreadParticipationTracker,
        non_conversational_messages: DiscordNonConversationalMessageTracker,
    ) -> Result<Self, GatewayError> {
        validate_discord_liveness_interval(config.liveness_interval_seconds)?;
        let effective_proxy = discord_effective_proxy(&config.proxy);
        validate_discord_proxy(&effective_proxy)?;
        config.proxy = effective_proxy.clone();
        let base = BasePlatformAdapter::new(&config.token).with_proxy(effective_proxy);

        base.validate_token()?;

        let client = base.build_client_without_system_proxy()?;
        let api_base_url = config
            .api_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DISCORD_API_BASE)
            .trim_end_matches('/')
            .to_string();

        let thread_participation = Arc::new(Mutex::new(thread_participation));
        let global_rate_limit = Arc::new(DiscordGlobalRateLimitBarrier::default());
        let gateway_driver = Arc::new(DiscordGatewayRuntime::new_with_global_rate_limit(
            &config,
            Arc::clone(&thread_participation),
            client.clone(),
            api_base_url.clone(),
            Arc::clone(&global_rate_limit),
        ));

        Ok(Self {
            base,
            config,
            api_base_url,
            client,
            gateway_driver,
            liveness_failed: Arc::new(AtomicBool::new(false)),
            liveness_task: Mutex::new(None),
            global_rate_limit,
            lifecycle: tokio::sync::Mutex::new(()),
            accepted_state_admission: Arc::new(DiscordAcceptedStateAdmission::default()),
            thread_participation,
            non_conversational_messages: Arc::new(Mutex::new(non_conversational_messages)),
            thread_participation_writer: Mutex::new(None),
            thread_participation_repair_pending: Arc::new(AtomicBool::new(false)),
            non_conversational_writer: Mutex::new(None),
            non_conversational_repair_pending: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &DiscordConfig {
        &self.config
    }

    /// Fetch Discord's optional identify budget before the socket worker starts.
    /// Failures leave the worker on its conservative single-shard five-second
    /// pacing rule.
    async fn configure_gateway_identify_session_start_limit(&self) {
        let deadline = tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT;
        let response = match self
            .send_rest_request_with_rate_limit_until("Discord gateway identify budget", || {
                self.client
                    .get(self.api_url("gateway/bot"))
                    .header("Authorization", self.auth_header())
            }, deadline)
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                debug!(
                    status = %response.status(),
                    "Discord gateway identify budget unavailable; using local pacing"
                );
                return;
            }
            Err(error) => {
                debug!(
                    error = %error,
                    "Discord gateway identify budget request failed; using local pacing"
                );
                return;
            }
        };
        let body: DiscordGatewayBotResponse = match self
            .response_json_with_deadline(response, deadline, "Discord gateway identify budget")
            .await
        {
            Ok(body) => body,
            Err(error) => {
                debug!(
                    error = %error,
                    "Discord gateway identify budget response was unusable; using local pacing"
                );
                return;
            }
        };
        let Some(limit) = body.session_start_limit else {
            return;
        };
        self.gateway_driver
            .configure_identify_session_start_limit(DiscordGatewaySessionStartLimit {
                total: limit.total,
                remaining: limit.remaining,
                reset_after_ms: limit.reset_after,
            })
            .await;
    }

    /// Attach CLI receivers that fsync raw ingress before allowing the
    /// Discord RESUME sequence to advance, then accept resolved messages.
    pub async fn set_durable_inbound_sender(
        &self,
        tx: tokio::sync::mpsc::Sender<DiscordInboundMessage>,
        journal_tx: tokio::sync::mpsc::Sender<DiscordIngressJournalEvent>,
    ) {
        self.gateway_driver
            .set_durable_inbound_sender(tx, journal_tx)
            .await;
    }

    /// Queue raw pre-policy journal entries for authoritative thread lookup
    /// and mention/channel admission after the socket has authenticated.
    pub fn queue_recovered_pre_policy_ingress(
        &self,
        entries: Vec<DiscordJournalEntry>,
    ) -> Result<(), GatewayError> {
        self.gateway_driver
            .queue_recovered_pre_policy_ingress(entries)
    }

    /// Snapshot retained gateway ingress for CLI-owned durable quarantine.
    ///
    /// The snapshot does not clear runtime state. Call
    /// [`Self::acknowledge_recovered_ingress_for_cli`] only after the spool
    /// write has completed successfully.
    pub fn snapshot_recovered_ingress_for_cli(&self) -> Vec<DiscordJournalEntry> {
        self.gateway_driver.snapshot_recovered_ingress_for_cli()
    }

    /// Release gateway ingress that the CLI has durably quarantined.
    pub fn acknowledge_recovered_ingress_for_cli(&self, message_ids: &[String]) {
        self.gateway_driver
            .acknowledge_recovered_ingress_for_cli(message_ids);
    }

    /// Stop socket intake and durably hand off any remaining ingress, while
    /// retaining outbound REST for the CLI-owned route drain. Final adapter
    /// shutdown happens only after that owner has completed.
    pub async fn quiesce_inbound(&self) -> Result<(), GatewayError> {
        // Serialize with PlatformAdapter::start: while socket intake is being
        // stopped, a reconnect watcher must not observe worker_alive=false and
        // resurrect a socket before the durable receiver pair is closed.
        let _lifecycle = self.lifecycle.lock().await;
        self.gateway_driver.stop_intake().await?;
        self.gateway_driver.close_inbound_sender().await;
        Ok(())
    }

    /// Abort the exact socket worker retained by the runtime and close the
    /// CLI delivery channel after a graceful quiesce deadline expires.
    pub async fn abort_inbound(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.close_accepted_state_admission();
        self.base.mark_stopped();
        self.gateway_driver.abort().await;
        self.stop_liveness_probe();
        self.gateway_driver.close_inbound_sender().await;
    }

    pub fn channel_controls(&self) -> &DiscordChannelControls {
        &self.config.channel_controls
    }

    pub fn should_ignore_channel(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.is_ignored(context)
    }

    pub fn should_auto_thread(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.should_auto_thread(context)
    }

    pub fn resolve_channel_skills(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
    ) -> Option<Vec<String>> {
        resolve_channel_skills_from_bindings(
            &self.config.channel_skill_bindings,
            channel_id,
            parent_id,
        )
    }

    pub fn thread_participation_contains(&self, thread_id: &str) -> bool {
        self.thread_participation
            .lock()
            .map(|tracker| tracker.contains(thread_id))
            .unwrap_or(false)
    }

    fn admit_accepted_state_commit(&self) -> Result<DiscordAcceptedStatePermit, GatewayError> {
        self.accepted_state_admission.admit()
    }

    fn close_accepted_state_admission(&self) {
        self.accepted_state_admission.close();
    }

    fn reopen_accepted_state_admission(&self) -> Result<(), GatewayError> {
        self.accepted_state_admission.reopen()
    }

    async fn wait_for_accepted_state_commits(&self) {
        self.accepted_state_admission.wait_until_drained().await;
    }

    fn thread_participation_writer_sender(
        &self,
    ) -> std::io::Result<
        tokio::sync::mpsc::UnboundedSender<
            DiscordTrackerWriteRequest<DiscordThreadParticipationTracker>,
        >,
    > {
        let mut slot = self
            .thread_participation_writer
            .lock()
            .map_err(|_| std::io::Error::other("discord thread writer lock poisoned"))?;
        if slot.is_none() {
            *slot = Some(DiscordTrackerWriter::start(
                Arc::clone(&self.thread_participation),
                Arc::clone(&self.thread_participation_repair_pending),
            ));
        }
        Ok(slot.as_ref().expect("writer initialized").sender())
    }

    fn started_thread_participation_writer_sender(
        &self,
    ) -> std::io::Result<
        Option<
            tokio::sync::mpsc::UnboundedSender<
                DiscordTrackerWriteRequest<DiscordThreadParticipationTracker>,
            >,
        >,
    > {
        self.thread_participation_writer
            .lock()
            .map(|slot| slot.as_ref().map(DiscordTrackerWriter::sender))
            .map_err(|_| std::io::Error::other("discord thread writer lock poisoned"))
    }

    async fn persist_thread_participation(
        &self,
        _permit: &DiscordAcceptedStatePermit,
        thread_id: &str,
    ) -> std::io::Result<bool> {
        let sender = self.thread_participation_writer_sender()?;
        let thread_id = thread_id.trim().to_string();
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(DiscordTrackerWriteRequest::Mutate {
                mutation: Box::new(move |tracker| tracker.stage_mark(thread_id)),
                acknowledgement,
            })
            .map_err(|_| std::io::Error::other("discord thread writer stopped"))?;
        receiver
            .await
            .map_err(|_| std::io::Error::other("discord thread writer stopped"))?
    }

    fn accepted_state_undurable(
        accepted_message_ids: Vec<String>,
        state_kind: &'static str,
        error: impl std::fmt::Display,
    ) -> GatewayError {
        GatewayError::AcceptedStateUndurable {
            accepted_message_ids,
            repair: format!(
                "retry durable Discord {state_kind} state commit without resending the accepted operation ({error})"
            ),
        }
    }

    async fn mark_conversational_thread_participation(
        &self,
        permit: &DiscordAcceptedStatePermit,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<(), GatewayError> {
        if metadata_marks_non_conversational(metadata) {
            return Ok(());
        }
        let Some(thread_id) = metadata
            .and_then(|metadata| metadata.thread_id.as_deref())
            .map(str::trim)
            .filter(|thread_id| !thread_id.is_empty())
        else {
            return Ok(());
        };
        self.persist_thread_participation(permit, thread_id)
            .await
            .map(|_| ())
            .map_err(|error| {
                Self::accepted_state_undurable(
                    vec![thread_id.to_string()],
                    "thread participation",
                    error,
                )
            })
    }

    pub fn non_conversational_message_contains(&self, message_id: &str) -> bool {
        self.non_conversational_messages
            .lock()
            .map(|tracker| tracker.contains(message_id))
            .unwrap_or(false)
    }

    fn non_conversational_writer_sender(
        &self,
    ) -> std::io::Result<
        tokio::sync::mpsc::UnboundedSender<
            DiscordTrackerWriteRequest<DiscordNonConversationalMessageTracker>,
        >,
    > {
        let mut slot = self
            .non_conversational_writer
            .lock()
            .map_err(|_| std::io::Error::other("discord non-conversational writer lock poisoned"))?;
        if slot.is_none() {
            *slot = Some(DiscordTrackerWriter::start(
                Arc::clone(&self.non_conversational_messages),
                Arc::clone(&self.non_conversational_repair_pending),
            ));
        }
        Ok(slot.as_ref().expect("writer initialized").sender())
    }

    fn started_non_conversational_writer_sender(
        &self,
    ) -> std::io::Result<
        Option<
            tokio::sync::mpsc::UnboundedSender<
                DiscordTrackerWriteRequest<DiscordNonConversationalMessageTracker>,
            >,
        >,
    > {
        self.non_conversational_writer
            .lock()
            .map(|slot| slot.as_ref().map(DiscordTrackerWriter::sender))
            .map_err(|_| std::io::Error::other("discord non-conversational writer lock poisoned"))
    }

    async fn persist_non_conversational_messages(
        &self,
        _permit: &DiscordAcceptedStatePermit,
        message_ids: Vec<String>,
    ) -> std::io::Result<bool> {
        let sender = self.non_conversational_writer_sender()?;
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(DiscordTrackerWriteRequest::Mutate {
                mutation: Box::new(move |tracker| tracker.stage_mark_many(message_ids)),
                acknowledgement,
            })
            .map_err(|_| std::io::Error::other("discord non-conversational writer stopped"))?;
        receiver
            .await
            .map_err(|_| std::io::Error::other("discord non-conversational writer stopped"))?
    }

    async fn mark_non_conversational_messages(
        &self,
        permit: &DiscordAcceptedStatePermit,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<(), GatewayError> {
        let message_ids = message_ids
            .into_iter()
            .map(|message_id| message_id.as_ref().trim().to_string())
            .filter(|message_id| !message_id.is_empty())
            .collect::<Vec<_>>();
        if message_ids.is_empty() {
            return Ok(());
        }
        self.persist_non_conversational_messages(permit, message_ids.clone())
            .await
            .map(|_| ())
            .map_err(|error| {
                Self::accepted_state_undurable(
                    message_ids,
                    "non-conversational message",
                    error,
                )
            })
    }

    /// Commit all local state that accompanies an accepted text chunk only
    /// after its response has been decoded. The caller converts a failure
    /// into a suffix-only partial record because the remote side effect has
    /// already happened.
    async fn mark_accepted_text_state(
        &self,
        permit: &DiscordAcceptedStatePermit,
        metadata: Option<&DiscordSendMetadata>,
        message_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.mark_conversational_thread_participation(permit, metadata)
            .await?;
        if metadata_marks_non_conversational(metadata) {
            if let Some(message_id) = message_id {
                self.mark_non_conversational_messages(permit, [message_id])
                    .await?;
            }
        }
        Ok(())
    }

    async fn flush_thread_participation_repair(&self) -> std::io::Result<bool> {
        let Some(sender) = self.started_thread_participation_writer_sender()? else {
            return Ok(false);
        };
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(DiscordTrackerWriteRequest::Flush { acknowledgement })
            .map_err(|_| std::io::Error::other("discord thread writer stopped"))?;
        receiver
            .await
            .map_err(|_| std::io::Error::other("discord thread writer stopped"))?
    }

    async fn flush_non_conversational_repair(&self) -> std::io::Result<bool> {
        let Some(sender) = self.started_non_conversational_writer_sender()? else {
            return Ok(false);
        };
        let (acknowledgement, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(DiscordTrackerWriteRequest::Flush { acknowledgement })
            .map_err(|_| std::io::Error::other("discord non-conversational writer stopped"))?;
        receiver
            .await
            .map_err(|_| std::io::Error::other("discord non-conversational writer stopped"))?
    }

    /// Enqueue FIFO barriers for every started tracker writer, even if its
    /// pending flag is not set yet. A caller may have been cancelled after
    /// enqueueing an accepted mutation but before its actor began the write.
    async fn flush_tracker_repairs(&self) -> Result<(), GatewayError> {
        self.flush_thread_participation_repair()
            .await
            .map_err(|error| {
                Self::accepted_state_undurable(Vec::new(), "thread participation", error)
            })?;
        self.flush_non_conversational_repair()
            .await
            .map_err(|error| {
                Self::accepted_state_undurable(Vec::new(), "non-conversational message", error)
            })?;
        Ok(())
    }

    /// Close and join writers only after their FIFO flush barriers completed.
    /// A later adapter start creates fresh writers from the committed snapshots.
    async fn shutdown_tracker_writers(&self) -> Result<(), GatewayError> {
        let thread_writer = self
            .thread_participation_writer
            .lock()
            .map_err(|_| {
                Self::accepted_state_undurable(
                    Vec::new(),
                    "thread participation",
                    "writer lock poisoned",
                )
            })?
            .take();
        if let Some(writer) = thread_writer {
            writer.shutdown().await.map_err(|error| {
                Self::accepted_state_undurable(Vec::new(), "thread participation", error)
            })?;
        }

        let non_conversational_writer = self
            .non_conversational_writer
            .lock()
            .map_err(|_| {
                Self::accepted_state_undurable(
                    Vec::new(),
                    "non-conversational message",
                    "writer lock poisoned",
                )
            })?
            .take();
        if let Some(writer) = non_conversational_writer {
            writer.shutdown().await.map_err(|error| {
                Self::accepted_state_undurable(Vec::new(), "non-conversational message", error)
            })?;
        }
        Ok(())
    }

    /// Return the authorization header value.
    fn auth_header(&self) -> String {
        format!("Bot {}", self.config.token)
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/{}", self.api_base_url, path.trim_start_matches('/'))
    }

    fn liveness_interval(&self) -> Option<std::time::Duration> {
        // Construction rejects invalid values. Keep this conversion fallible
        // anyway so a future unchecked config path can only disable probing,
        // never panic after socket startup.
        std::time::Duration::try_from_secs_f64(self.config.liveness_interval_seconds)
            .ok()
            .filter(|interval| !interval.is_zero())
    }

    fn liveness_threshold(&self) -> Option<u32> {
        (self.config.liveness_failure_threshold > 0)
            .then_some(self.config.liveness_failure_threshold)
    }


    fn start_liveness_probe(&self) {
        let Some(interval) = self.liveness_interval() else {
            return;
        };
        let Some(threshold) = self.liveness_threshold() else {
            return;
        };
        let Ok(mut slot) = self.liveness_task.lock() else {
            debug!("discord liveness task lock poisoned");
            return;
        };
        if !self.base.is_running() {
            return;
        }
        if slot.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        self.liveness_failed.store(false, Ordering::SeqCst);
        let client = self.client.clone();
        let token = self.config.token.clone();
        let api_base_url = self.api_base_url.clone();
        let failed = Arc::clone(&self.liveness_failed);
        let stop = self.gateway_driver.stop_receiver();
        let global_rate_limit = Arc::clone(&self.global_rate_limit);
        *slot = Some(tokio::spawn(async move {
            run_discord_liveness_probe_loop(
                client,
                token,
                api_base_url,
                interval,
                threshold,
                failed,
                stop,
                global_rate_limit,
            )
            .await;
        }));
    }

    fn stop_liveness_probe(&self) {
        self.liveness_failed.store(false, Ordering::SeqCst);
        let Ok(mut slot) = self.liveness_task.lock() else {
            debug!("discord liveness task lock poisoned");
            return;
        };
        if let Some(task) = slot.take() {
            task.abort();
        }
    }

    // -----------------------------------------------------------------------

    /// Submit a REST operation with a bounded, stop-aware Discord rate-limit
    /// retry. The request is rebuilt for each attempt so multipart and JSON
    /// bodies are never reused after reqwest has consumed them.
    async fn send_rest_request_with_rate_limit<F>(
        &self,
        operation: &str,
        make_request: F,
    ) -> Result<reqwest::Response, GatewayError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        self.send_rest_request_with_rate_limit_until(
            operation,
            make_request,
            tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT,
        )
        .await
    }

    async fn send_rest_request_with_rate_limit_until<F>(
        &self,
        operation: &str,
        make_request: F,
        deadline: tokio::time::Instant,
    ) -> Result<reqwest::Response, GatewayError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        self.send_rest_request_with_rate_limit_with_deadlines(
            operation,
            make_request,
            deadline,
            deadline,
        )
        .await
    }
    /// Rate-limit admission may use a much shorter deadline than the actual
    /// HTTP exchange (notably for multipart uploads).
    async fn send_rest_request_with_rate_limit_with_deadlines<F>(
        &self,
        operation: &str,
        mut make_request: F,
        rate_limit_deadline: tokio::time::Instant,
        request_deadline: tokio::time::Instant,
    ) -> Result<reqwest::Response, GatewayError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let mut stop = self.gateway_driver.stop_receiver();
        loop {
            // Every attempt—initial, local retry, and a different outbound
            // operation—must pass the account-wide global 429 admission gate.
            self.global_rate_limit
                .wait_until_open(rate_limit_deadline, &mut stop, operation)
                .await?;
            if *stop.borrow() {
                return Err(GatewayError::SendFailed(format!(
                    "{operation} cancelled because the Discord adapter is stopping"
                )));
            }
            let rate_limit_remaining =
                rate_limit_deadline.saturating_duration_since(tokio::time::Instant::now());
            if rate_limit_remaining.is_zero() {
                return Err(GatewayError::RateLimited {
                    retry_after: Some(std::time::Duration::ZERO),
                });
            }
            // RequestBuilder's timeout covers the complete HTTP exchange,
            // including a response body that has not yet arrived. Pair it
            // with the stop receiver so adapter shutdown never waits for a
            // blocked REST transport.
            let request_remaining =
                request_deadline.saturating_duration_since(tokio::time::Instant::now());
            if request_remaining.is_zero() {
                return Err(GatewayError::SendFailed(format!(
                    "{operation} exceeded its Discord request delivery deadline"
                )));
            }
            let request = make_request().timeout(request_remaining);
            let response = tokio::select! {
                response = request.send() => response.map_err(|error| {
                    GatewayError::SendFailed(format!("{operation} failed: {error}"))
                })?,
                changed = stop.changed() => {
                    let _ = changed;
                    return Err(GatewayError::SendFailed(format!(
                        "{operation} cancelled because the Discord adapter is stopping"
                    )));
                }
            };
            if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Ok(response);
            }

            // Preserve all header evidence before body consumption. A stalled
            // 429 body must remain a typed, paced retry—not a zero-delay
            // retry or liveness failure.
            let header_retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<f64>().ok())
                .and_then(discord_retry_after_duration);
            let header_global = discord_rate_limit_is_global_header(response.headers());
            let header_delay =
                header_retry_after.unwrap_or(std::time::Duration::from_secs(1));
            if header_global {
                self.global_rate_limit.extend(header_delay);
            }
            let body = tokio::select! {
                body = discord_response_text_limited(response) => body,
                _ = tokio::time::sleep_until(rate_limit_deadline) => {
                    return Err(GatewayError::RateLimited {
                        retry_after: Some(header_delay),
                    });
                }
                changed = stop.changed() => {
                    let _ = changed;
                    // Discord already returned a 429 before this body wait,
                    // so this route has a proven no-side-effect retry path.
                    return Err(GatewayError::RateLimited {
                        retry_after: Some(header_delay),
                    });
                }
            };
            let (retry_after, body_global) =
                discord_rate_limit_delay(&body, header_retry_after);
            if header_global || body_global {
                // The header barrier is published before body consumption;
                // a longer JSON hint only extends it.
                self.global_rate_limit.extend(retry_after);
            }
            let rate_limit_remaining =
                rate_limit_deadline.saturating_duration_since(tokio::time::Instant::now());
            if retry_after > rate_limit_remaining {
                return Err(GatewayError::RateLimited {
                    retry_after: Some(retry_after),
                });
            }

            tokio::select! {
                _ = tokio::time::sleep(retry_after) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        // The immediately preceding response was a 429, so a
                        // shutdown during its retry delay remains safely
                        // resumable rather than an ambiguous message POST.
                        return Err(GatewayError::RateLimited {
                            retry_after: Some(retry_after),
                        });
                    }
                }
            }
        }
    }

    async fn response_text_with_deadline(
        &self,
        response: reqwest::Response,
        deadline: tokio::time::Instant,
        operation: &str,
    ) -> Result<String, GatewayError> {
        let mut stop = self.gateway_driver.stop_receiver();
        if *stop.borrow() {
            return Err(GatewayError::SendFailed(format!(
                "{operation} cancelled because the Discord adapter is stopping"
            )));
        }
        tokio::select! {
            text = discord_response_text_limited(response) => Ok(text),
            _ = tokio::time::sleep_until(deadline) => Err(GatewayError::SendFailed(format!(
                "{operation} response body exceeded the Discord delivery deadline"
            ))),
            changed = stop.changed() => {
                let _ = changed;
                Err(GatewayError::SendFailed(format!(
                    "{operation} cancelled because the Discord adapter is stopping"
                )))
            }
        }
    }

    async fn response_json_with_deadline<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
        deadline: tokio::time::Instant,
        operation: &str,
    ) -> Result<T, GatewayError> {
        let mut stop = self.gateway_driver.stop_receiver();
        if *stop.borrow() {
            return Err(GatewayError::SendFailed(format!(
                "{operation} cancelled because the Discord adapter is stopping"
            )));
        }
        tokio::select! {
            json = discord_response_json_limited::<T>(response) => json,
            _ = tokio::time::sleep_until(deadline) => Err(GatewayError::SendFailed(format!(
                "{operation} response body exceeded the Discord delivery deadline"
            ))),
            changed = stop.changed() => {
                let _ = changed;
                Err(GatewayError::SendFailed(format!(
                    "{operation} cancelled because the Discord adapter is stopping"
                )))
            }
        }
    }

    fn text_delivery_failure(
        channel_id: &str,
        metadata: Option<&DiscordSendMetadata>,
        reply_to_mode: DiscordReplyToMode,
        start_chunk_index: usize,
        chunks: &[String],
        failed_chunk: usize,
        accepted_chunks: usize,
        accepted_message_ids: &[String],
        error: GatewayError,
    ) -> GatewayError {
        // A zero-prefix rate-limit is still a concrete continuation: gateway
        // routing has already produced the response, so whole-turn replay
        // would be wrong. Transport failures remain non-retryable partial
        // records because acceptance is ambiguous even before an ID is known.
        let retryable = matches!(
            error.send_error_kind(),
            hermes_core::errors::SendErrorKind::RateLimited
        );
        let retry_after = match &error {
            GatewayError::RateLimited { retry_after } => *retry_after,
            _ => None,
        };
        GatewayError::PartialDelivery(hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: channel_id.to_string(),
            thread_id: metadata.and_then(|metadata| metadata.thread_id.clone()),
            reply_to_message_id: metadata.and_then(|metadata| metadata.reply_to_message_id.clone()),
            reply_to_mode: reply_to_mode.as_str().to_string(),
            non_conversational: metadata_marks_non_conversational(metadata),
            next_chunk_index: start_chunk_index.saturating_add(failed_chunk),
            remaining_text: chunks[failed_chunk..].concat(),
            accepted_message_ids: accepted_message_ids.to_vec(),
            accepted_chunks,
            retryable,
            retry_after,
            failure: error.to_string(),
            repair_required: None,
        })
    }

    /// The provider accepted the current chunk, but its required local state
    /// could not commit. Retain only the exact unsent suffix; the accepted
    /// prefix is never replayed.
    fn text_accepted_state_failure(
        channel_id: &str,
        metadata: Option<&DiscordSendMetadata>,
        reply_to_mode: DiscordReplyToMode,
        start_chunk_index: usize,
        chunks: &[String],
        next_unsent_chunk: usize,
        accepted_chunks: usize,
        accepted_message_ids: &[String],
        error: GatewayError,
    ) -> GatewayError {
        let repair_required = error.to_string();
        GatewayError::PartialDelivery(hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: channel_id.to_string(),
            thread_id: metadata.and_then(|metadata| metadata.thread_id.clone()),
            reply_to_message_id: metadata.and_then(|metadata| metadata.reply_to_message_id.clone()),
            reply_to_mode: reply_to_mode.as_str().to_string(),
            non_conversational: metadata_marks_non_conversational(metadata),
            next_chunk_index: start_chunk_index.saturating_add(next_unsent_chunk),
            remaining_text: chunks[next_unsent_chunk..].concat(),
            accepted_message_ids: accepted_message_ids.to_vec(),
            accepted_chunks,
            retryable: true,
            retry_after: None,
            failure: "Discord accepted an outbound chunk but its local state requires repair"
                .to_string(),
            repair_required: Some(repair_required),
        })
    }

    /// A message-creating POST may have reached Discord even when its
    /// transport or success body is unavailable. Return a non-retryable
    /// durable record rather than a raw error that would invite whole-message
    /// replay and duplicate the side effect.
    fn uncertain_message_post_failure(
        &self,
        channel_id: &str,
        metadata: Option<&DiscordSendMetadata>,
        accepted: bool,
        error: GatewayError,
    ) -> GatewayError {
        let retry_after = match &error {
            GatewayError::RateLimited { retry_after } => *retry_after,
            _ => None,
        };
        GatewayError::PartialDelivery(hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: channel_id.to_string(),
            thread_id: metadata.and_then(|metadata| metadata.thread_id.clone()),
            reply_to_message_id: metadata.and_then(|metadata| metadata.reply_to_message_id.clone()),
            reply_to_mode: DiscordReplyToMode::parse(Some(&self.config.reply_to_mode))
                .as_str()
                .to_string(),
            non_conversational: metadata_marks_non_conversational(metadata),
            next_chunk_index: usize::from(accepted),
            remaining_text: String::new(),
            accepted_message_ids: Vec::new(),
            accepted_chunks: usize::from(accepted),
            retryable: false,
            retry_after,
            failure: error.to_string(),
            repair_required: None,
        })
    }

    /// Classify a message-creating POST by whether request construction was
    /// reached. Pre-request rate-limit/stop/deadline failures are safe to
    /// retry; once `send` has begun, a transport failure is ambiguous and must
    /// be quarantined as a non-retryable partial delivery.
    async fn send_message_post_with_rate_limit_until<F>(
        &self,
        operation: &str,
        channel_id: &str,
        metadata: Option<&DiscordSendMetadata>,
        deadline: tokio::time::Instant,
        make_request: F,
    ) -> Result<reqwest::Response, GatewayError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        self.send_message_post_with_rate_limit_with_deadlines(
            operation,
            channel_id,
            metadata,
            deadline,
            deadline,
            make_request,
        )
        .await
    }
    async fn send_message_post_with_rate_limit_with_deadlines<F>(
        &self,
        operation: &str,
        channel_id: &str,
        metadata: Option<&DiscordSendMetadata>,
        rate_limit_deadline: tokio::time::Instant,
        request_deadline: tokio::time::Instant,
        mut make_request: F,
    ) -> Result<reqwest::Response, GatewayError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let request_started = std::sync::atomic::AtomicBool::new(false);
        match self
            .send_rest_request_with_rate_limit_with_deadlines(
                operation,
                || {
                    request_started.store(true, Ordering::Relaxed);
                    make_request()
                },
                rate_limit_deadline,
                request_deadline,
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(error @ GatewayError::RateLimited { .. }) => Err(error),
            Err(error) if request_started.load(Ordering::Relaxed) => Err(
                self.uncertain_message_post_failure(channel_id, metadata, false, error),
            ),
            Err(error) => Err(error),
        }
    }
    // REST API: Sending messages
    // -----------------------------------------------------------------------

    /// Send a message to a Discord channel, splitting if it exceeds 2000 chars.
    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.send_text_with_metadata(channel_id, content, None)
            .await
    }

    /// Send a message, honoring Discord thread routing metadata when present.
    pub async fn send_text_with_metadata(
        &self,
        channel_id: &str,
        content: &str,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<Vec<String>, GatewayError> {
        self.send_text_with_metadata_at_chunk(
            channel_id,
            content,
            metadata,
            0,
            DiscordReplyToMode::parse(Some(&self.config.reply_to_mode)),
            false,
        )
        .await
    }

    async fn send_text_with_metadata_at_chunk(
        &self,
        channel_id: &str,
        content: &str,
        metadata: Option<&DiscordSendMetadata>,
        start_chunk_index: usize,
        reply_to_mode: DiscordReplyToMode,
        content_is_preformatted: bool,
    ) -> Result<Vec<String>, GatewayError> {
        // One budget spans every chunk and reply-reference retry. Returning a
        // PartialDelivery before Gateway's outer commit timeout preserves an
        // accepted prefix instead of letting cancellation erase the suffix.
        let delivery_deadline =
            tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT;
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let formatted_content = content_is_preformatted
            .then(|| content.to_string())
            .unwrap_or_else(|| format_discord_outgoing_content(content));
        let chunks = split_message(&formatted_content, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();
        let mut accepted_chunks = 0usize;
        let reply_to_message_id = reply_to_message_id_for_metadata(metadata);
        let mut suppress_reply_references = false;

        for (index, chunk) in chunks.iter().enumerate() {
            // Admit one remote side effect at a time. Once stop closes this
            // gate, an already accepted chunk can durably finish but no later
            // chunk can begin.
            let accepted_state_permit = self.admit_accepted_state_commit()?;
            let chunk_index = start_chunk_index.saturating_add(index);
            let url = self.api_url(&format!("channels/{}/messages", target_channel_id));
            let include_reply_reference = !suppress_reply_references
                && reply_to_message_id.is_some()
                && reply_to_mode.references_chunk(chunk_index);
            let body = discord_message_body(
                chunk,
                include_reply_reference.then_some(reply_to_message_id.unwrap_or_default()),
                default_discord_allowed_mentions(),
            );

            let resp = match self
                .send_message_post_with_rate_limit_until(
                    "Discord send",
                    channel_id,
                    metadata,
                    delivery_deadline,
                    || {
                        self.client
                            .post(&url)
                            .header("Authorization", self.auth_header())
                            .header("Content-Type", "application/json")
                            .json(&body)
                    },
                )
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    return Err(Self::text_delivery_failure(
                        channel_id,
                        metadata,
                        reply_to_mode,
                        start_chunk_index,
                        &chunks,
                        index,
                        accepted_chunks,
                        &message_ids,
                        error,
                    ));
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let text = match self
                    .response_text_with_deadline(resp, delivery_deadline, "Discord send")
                    .await
                {
                    Ok(text) => text,
                    Err(error) => {
                        return Err(Self::text_delivery_failure(
                            channel_id,
                            metadata,
                            reply_to_mode,
                            start_chunk_index,
                            &chunks,
                            index,
                            accepted_chunks,
                            &message_ids,
                            error,
                        ));
                    }
                };
                if include_reply_reference && discord_reply_reference_error_allows_retry(&text) {
                    suppress_reply_references = true;
                    let retry_body =
                        discord_message_body(chunk, None, default_discord_allowed_mentions());
                    let retry_resp = match self
                        .send_message_post_with_rate_limit_until(
                            "Discord send",
                            channel_id,
                            metadata,
                            delivery_deadline,
                            || {
                                self.client
                                    .post(&url)
                                    .header("Authorization", self.auth_header())
                                    .header("Content-Type", "application/json")
                                    .json(&retry_body)
                            },
                        )
                        .await
                    {
                        Ok(response) => response,
                        Err(error) => {
                            return Err(Self::text_delivery_failure(
                                channel_id,
                                metadata,
                                reply_to_mode,
                                start_chunk_index,
                                &chunks,
                                index,
                                accepted_chunks,
                                &message_ids,
                                error,
                            ));
                        }
                    };

                    if !retry_resp.status().is_success() {
                        let retry_status = retry_resp.status();
                        let retry_text = match self
                            .response_text_with_deadline(
                                retry_resp,
                                delivery_deadline,
                                "Discord send",
                            )
                            .await
                        {
                            Ok(text) => text,
                            Err(error) => {
                                return Err(Self::text_delivery_failure(
                                    channel_id,
                                    metadata,
                                    reply_to_mode,
                                    start_chunk_index,
                                    &chunks,
                                    index,
                                    accepted_chunks,
                                    &message_ids,
                                    error,
                                ));
                            }
                        };
                        return Err(Self::text_delivery_failure(
                            channel_id,
                            metadata,
                            reply_to_mode,
                            start_chunk_index,
                            &chunks,
                            index,
                            accepted_chunks,
                            &message_ids,
                            discord_http_status_error(retry_status, retry_text),
                        ));
                    }

                    accepted_chunks += 1;
                    let accepted_message_id = match self
                        .response_json_with_deadline::<DiscordMessage>(
                            retry_resp,
                            delivery_deadline,
                            "Discord send",
                        )
                        .await
                    {
                        Ok(message) => {
                            message_ids.push(message.id);
                            message_ids.last().map(String::as_str)
                        }
                        Err(error) => {
                            warn!(
                                chunk_index = chunk_index,
                                error = %error,
                                "Discord accepted a retry message but did not return a usable message ID"
                            );
                            None
                        }
                    };
                    if let Err(error) = self
                        .mark_accepted_text_state(&accepted_state_permit, metadata, accepted_message_id)
                        .await
                    {
                        return Err(Self::text_accepted_state_failure(
                            channel_id,
                            metadata,
                            reply_to_mode,
                            start_chunk_index,
                            &chunks,
                            index.saturating_add(1),
                            accepted_chunks,
                            &message_ids,
                            error,
                        ));
                    }
                    continue;
                }

                return Err(Self::text_delivery_failure(
                    channel_id,
                    metadata,
                    reply_to_mode,
                    start_chunk_index,
                    &chunks,
                    index,
                    accepted_chunks,
                    &message_ids,
                    discord_http_status_error(status, text),
                ));
            }

            accepted_chunks += 1;
            let accepted_message_id = match self
                .response_json_with_deadline::<DiscordMessage>(resp, delivery_deadline, "Discord send")
                .await
            {
                Ok(message) => {
                    message_ids.push(message.id);
                    message_ids.last().map(String::as_str)
                }
                Err(error) => {
                    warn!(
                        chunk_index = chunk_index,
                        error = %error,
                        "Discord accepted a message but did not return a usable message ID"
                    );
                    None
                }
            };
            if let Err(error) = self
                .mark_accepted_text_state(&accepted_state_permit, metadata, accepted_message_id)
                .await
            {
                return Err(Self::text_accepted_state_failure(
                    channel_id,
                    metadata,
                    reply_to_mode,
                    start_chunk_index,
                    &chunks,
                    index.saturating_add(1),
                    accepted_chunks,
                    &message_ids,
                    error,
                ));
            }
        }


        Ok(message_ids)
    }

    /// Resume only a Discord suffix whose prior failure proved it was not
    /// accepted. The stored chunk index preserves `first`/`all` reply
    /// reference behavior across the split boundary.
    pub async fn resume_partial_text(
        &self,
        partial: &hermes_core::errors::PartialDelivery,
    ) -> Result<(), GatewayError> {
        if partial.platform != "discord" || !partial.retryable || partial.repair_required.is_some() {
            return Err(GatewayError::PartialDelivery(partial.clone()));
        }
        let metadata = DiscordSendMetadata {
            thread_id: partial.thread_id.clone(),
            reply_to_message_id: partial.reply_to_message_id.clone(),
            non_conversational: partial.non_conversational,
        };
        match self
            .send_text_with_metadata_at_chunk(
                &partial.chat_id,
                &partial.remaining_text,
                Some(&metadata),
                partial.next_chunk_index,
                DiscordReplyToMode::parse(Some(&partial.reply_to_mode)),
                true,
            )
            .await
        {
            Ok(_) => Ok(()),
            // A resumed suffix may itself have accepted one or more chunks
            // before a later 429. Preserve that delta for the durable owner;
            // replacing it with the prior cursor would visibly duplicate the
            // accepted resumed prefix.
            Err(GatewayError::PartialDelivery(next)) => {
                Err(GatewayError::PartialDelivery(next))
            }
            // Preserve a zero-progress server hint rather than reducing it to
            // a display string or retaining an obsolete prior delay.
            Err(GatewayError::RateLimited { retry_after }) => {
                let mut continuation = partial.clone();
                continuation.retry_after = retry_after;
                Err(GatewayError::PartialDelivery(continuation))
            }
            Err(error)
                if matches!(
                    error.send_error_kind(),
                    hermes_core::errors::SendErrorKind::RateLimited
                ) =>
            {
                Err(GatewayError::PartialDelivery(partial.clone()))
            }
            Err(error) => Err(error),
        }
    }

    /// Create a Discord forum post thread from message content.
    ///
    /// Follow-up chunks are sent to the created thread. If the starter post is
    /// created but a follow-up chunk fails, the successful starter message is
    /// returned together with warnings, matching the upstream partial-send
    /// behavior.
    pub async fn send_forum_post(
        &self,
        forum_channel_id: &str,
        content: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordForumSendOutcome, GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let Some(first_chunk) = chunks.first() else {
            return Err(GatewayError::SendFailed(
                "Discord forum post requires content".into(),
            ));
        };
        let accepted_state_permit = self.admit_accepted_state_commit()?;
        let deadline =
            tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT;
        let url = self.api_url(&format!("channels/{}/threads", forum_channel_id));
        let body = forum_thread_payload(first_chunk, None, auto_archive_duration);

        let resp = self
            .send_message_post_with_rate_limit_until(
                "Discord forum post",
                forum_channel_id,
                None,
                deadline,
                || {
                    self.client
                        .post(&url)
                        .header("Authorization", self.auth_header())
                        .header("Content-Type", "application/json")
                        .json(&body)
                },
            )
            .await?;

        if !resp.status().is_success() {
            let text = self
                .response_text_with_deadline(resp, deadline, "Discord forum post")
                .await?;
            return Err(GatewayError::SendFailed(format!(
                "Discord forum post API error: {}",
                text
            )));
        }

        let thread: DiscordForumThread = self
            .response_json_with_deadline(resp, deadline, "Discord forum post")
            .await
            .map_err(|error| {
                self.uncertain_message_post_failure(forum_channel_id, None, true, error)
            })?;
        let thread_metadata = DiscordSendMetadata::with_thread_id(thread.id.clone());
        self.mark_conversational_thread_participation(&accepted_state_permit, Some(&thread_metadata))
            .await?;
        let message_id = thread
            .message
            .as_ref()
            .map(|message| message.id.clone())
            .unwrap_or_else(|| thread.id.clone());
        let mut warnings = Vec::new();

        for chunk in chunks.iter().skip(1) {
            let metadata = DiscordSendMetadata::with_thread_id(thread.id.clone());
            if let Err(err) = self
                .send_text_with_metadata(forum_channel_id, chunk, Some(&metadata))
                .await
            {
                warnings.push(err.to_string());
            }
        }

        Ok(DiscordForumSendOutcome {
            thread_id: thread.id,
            message_id,
            warnings,
        })
    }

    /// Edit an existing message in a Discord channel.
    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = self.api_url(&format!("channels/{}/messages/{}", channel_id, message_id));

        let formatted_content = format_discord_outgoing_content(content);
        let body = with_default_allowed_mentions(serde_json::json!({
            "content": truncate_discord_content(&formatted_content, MAX_MESSAGE_LENGTH),
        }));

        let resp = self
            .send_rest_request_with_rate_limit("Discord edit", || {
                self.client
                    .patch(&url)
                    .header("Authorization", self.auth_header())
                    .header("Content-Type", "application/json")
                    .json(&body)
            })
            .await?;

        if !resp.status().is_success() {
            let text = discord_response_text_limited(resp).await;
            return Err(GatewayError::SendFailed(format!(
                "Discord edit API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Embeds
    // -----------------------------------------------------------------------

    /// Send a message with one or more embeds to a Discord channel.
    pub async fn send_embed(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
    ) -> Result<String, GatewayError> {
        self.send_embed_with_metadata(channel_id, content, embeds, None)
            .await
    }

    /// Send embeds, honoring Discord thread routing metadata when present.
    pub async fn send_embed_with_metadata(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.send_embed_with_metadata_until(
            channel_id,
            content,
            embeds,
            metadata,
            tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT,
        )
        .await
    }

    async fn send_embed_with_metadata_until(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
        metadata: Option<&DiscordSendMetadata>,
        deadline: tokio::time::Instant,
    ) -> Result<String, GatewayError> {
        let accepted_state_permit = self.admit_accepted_state_commit()?;
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = self.api_url(&format!("channels/{}/messages", target_channel_id));

        let mut body = with_default_allowed_mentions(serde_json::json!({ "embeds": embeds }));
        if let Some(text) = content {
            body["content"] = serde_json::Value::String(text.to_string());
        }

        let resp = self
            .send_message_post_with_rate_limit_until(
                "Discord embed send",
                channel_id,
                metadata,
                deadline,
                || {
                    self.client
                        .post(&url)
                        .header("Authorization", self.auth_header())
                        .header("Content-Type", "application/json")
                        .json(&body)
                },
            )
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = self
                .response_text_with_deadline(resp, deadline, "Discord embed send")
                .await?;
            return Err(discord_media_http_status_error(status, text));
        }

        // A successful status means Discord has already accepted the side
        // effect even if the body stalls or fails to decode.
        self.mark_conversational_thread_participation(&accepted_state_permit, metadata)
            .await?;
        let msg: DiscordMessage = self
            .response_json_with_deadline(resp, deadline, "Discord embed send")
            .await
            .map_err(|error| {
                self.uncertain_message_post_failure(channel_id, metadata, true, error)
            })?;

        if metadata_marks_non_conversational(metadata) {
            self.mark_non_conversational_messages(&accepted_state_permit, [msg.id.as_str()])
                .await?;
        }

        Ok(msg.id)
    }

    // -----------------------------------------------------------------------
    // REST API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Discord channel using multipart form data.
    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, file_path, caption, None)
            .await
    }

    /// Upload a file, honoring Discord thread routing metadata when present.
    pub async fn upload_file_with_metadata(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let now = tokio::time::Instant::now();
        self.upload_file_with_metadata_until(
            channel_id,
            file_path,
            caption,
            metadata,
            now + DISCORD_REST_RATE_LIMIT_MAX_WAIT,
            now + DISCORD_UPLOAD_TRANSFER_MAX_WAIT,
        )
        .await
    }

    /// The admission deadline bounds global/local 429 waits; the transfer
    /// deadline separately covers the multipart HTTP exchange and response.
    async fn upload_file_with_metadata_until(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
        rate_limit_deadline: tokio::time::Instant,
        transfer_deadline: tokio::time::Instant,
    ) -> Result<String, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = self.api_url(&format!("channels/{}/messages", target_channel_id));

        let file_metadata = tokio::fs::metadata(file_path)
            .await
            .map_err(|error| discord_local_media_preflight_failure(file_path, &error))?;
        if file_metadata.len() > DISCORD_UPLOAD_FILE_LIMIT_BYTES {
            return Err(discord_oversized_media_preflight_failure(
                file_path,
                file_metadata.len(),
            ));
        }

        // Do not trust metadata alone: a concurrent writer can grow a file
        // after the preflight check. The bounded read keeps the allocation and
        // retry payload within Discord's attachment cap either way.
        use tokio::io::AsyncReadExt as _;
        let file = tokio::fs::File::open(file_path)
            .await
            .map_err(|error| discord_local_media_preflight_failure(file_path, &error))?;
        let mut bounded = file.take(DISCORD_UPLOAD_FILE_LIMIT_BYTES.saturating_add(1));
        let mut buffer = Vec::with_capacity(file_metadata.len() as usize);
        bounded
            .read_to_end(&mut buffer)
            .await
            .map_err(|error| discord_local_media_preflight_failure(file_path, &error))?;
        if buffer.len() as u64 > DISCORD_UPLOAD_FILE_LIMIT_BYTES {
            return Err(discord_oversized_media_preflight_failure(
                file_path,
                buffer.len() as u64,
            ));
        }
        // `Bytes` retains the owned Vec and clones by reference count, so a
        // Discord 429 retry does not duplicate an attachment-sized buffer.
        let file_bytes = bytes::Bytes::from(buffer);

        let file_name = discord_safe_attachment_file_name(file_path);

        let payload = with_default_allowed_mentions(match caption {
            Some(cap) => serde_json::json!({ "content": cap }),
            None => serde_json::json!({}),
        });
        let payload_json = payload.to_string();
        let accepted_state_permit = self.admit_accepted_state_commit()?;
        let resp = self
            .send_message_post_with_rate_limit_with_deadlines(
                "Discord file upload",
                channel_id,
                metadata,
                rate_limit_deadline,
                transfer_deadline,
                || {
                    let part = reqwest::multipart::Part::stream_with_length(
                        file_bytes.clone(),
                        file_bytes.len() as u64,
                    )
                    .file_name(file_name.clone());
                    let form = reqwest::multipart::Form::new()
                        .part("files[0]", part)
                        .text("payload_json", payload_json.clone());
                    self.client
                        .post(&url)
                        .header("Authorization", self.auth_header())
                        .multipart(form)
                },
            )
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = self
                .response_text_with_deadline(resp, transfer_deadline, "Discord file upload")
                .await?;
            return Err(discord_media_http_status_error(status, text));
        }

        self.mark_conversational_thread_participation(&accepted_state_permit, metadata)
            .await?;
        let msg: DiscordMessage = self
            .response_json_with_deadline(resp, transfer_deadline, "Discord file upload")
            .await
            .map_err(|error| {
                self.uncertain_message_post_failure(channel_id, metadata, true, error)
            })?;

        if metadata_marks_non_conversational(metadata) {
            self.mark_non_conversational_messages(&accepted_state_permit, [msg.id.as_str()])
                .await?;
        }

        Ok(msg.id)
    }

    /// Send a local image file as a Discord attachment.
    pub async fn send_image_file(
        &self,
        channel_id: &str,
        image_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, image_path, caption, metadata)
            .await
    }

    /// Send an image URL as a Discord embed.
    pub async fn send_image(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.send_image_url_with_metadata(channel_id, image_url, caption, metadata)
            .await
    }

    /// Send a voice/audio file as a Discord attachment.
    pub async fn send_voice(
        &self,
        channel_id: &str,
        audio_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, audio_path, caption, metadata)
            .await
    }

    /// Send an image URL as an embed, honoring thread routing metadata.
    pub async fn send_image_url_with_metadata(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let mut embed = DiscordEmbed::new();
        embed.image = Some(EmbedMedia {
            url: image_url.to_string(),
        });
        self.send_embed_with_metadata(channel_id, caption, &[embed], metadata)
            .await
    }

    // -----------------------------------------------------------------------
    // REST API: Reactions
    // -----------------------------------------------------------------------

    /// Add a reaction to a message.
    ///
    /// `emoji` should be a URL-encoded unicode emoji (e.g. `%F0%9F%91%8D`)
    /// or a custom emoji in the form `name:id`.
    pub async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = self.api_url(&format!(
            "channels/{}/messages/{}/reactions/{}/@me",
            channel_id, message_id, emoji
        ));

        let resp = self
            .send_rest_request_with_rate_limit("Discord add_reaction", || {
                self.client
                    .put(&url)
                    .header("Authorization", self.auth_header())
                    .header("Content-Length", "0")
            })
            .await?;

        if !resp.status().is_success() {
            let text = discord_response_text_limited(resp).await;
            return Err(GatewayError::SendFailed(format!(
                "Discord add_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    /// Remove the bot's own reaction from a message.
    pub async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = self.api_url(&format!(
            "channels/{}/messages/{}/reactions/{}/@me",
            channel_id, message_id, emoji
        ));

        let resp = self
            .send_rest_request_with_rate_limit("Discord remove_reaction", || {
                self.client
                    .delete(&url)
                    .header("Authorization", self.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            let text = discord_response_text_limited(resp).await;
            return Err(GatewayError::SendFailed(format!(
                "Discord remove_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Threads
    // -----------------------------------------------------------------------

    /// Create or recover the unique public thread attached to an existing message.
    ///
    /// Discord assigns the source message ID to the created thread. A repeated
    /// POST returns error code 160004, so restart-safe retries recover and
    /// validate that stable channel instead of creating local cache truth.
    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, GatewayError> {
        self.create_thread_classified(channel_id, message_id, name, auto_archive_duration)
            .await
            .map_err(|failure| match failure {
                DiscordThreadCreationFailure::Rejected(error)
                | DiscordThreadCreationFailure::Retryable(error)
                | DiscordThreadCreationFailure::Ambiguous(error) => error,
            })
    }

    async fn create_thread_classified(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, DiscordThreadCreationFailure> {
        let accepted_state_permit = self
            .admit_accepted_state_commit()
            .map_err(DiscordThreadCreationFailure::Retryable)?;
        self.create_thread_classified_with_permit(
            channel_id,
            message_id,
            name,
            auto_archive_duration,
            &accepted_state_permit,
        )
        .await
    }

    async fn create_thread_classified_with_permit(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
        _permit: &DiscordAcceptedStatePermit,
    ) -> Result<DiscordThread, DiscordThreadCreationFailure> {
        self.create_thread_classified_until(
            channel_id,
            message_id,
            name,
            auto_archive_duration,
            tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT,
        )
        .await
    }

    async fn create_thread_classified_until(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
        deadline: tokio::time::Instant,
    ) -> Result<DiscordThread, DiscordThreadCreationFailure> {
        let url = self.api_url(&format!(
            "channels/{}/messages/{}/threads",
            channel_id, message_id
        ));

        let mut body = serde_json::json!({ "name": name });
        if let Some(dur) = auto_archive_duration {
            body["auto_archive_duration"] = serde_json::Value::Number(dur.into());
        }

        let resp = match self
            .send_rest_request_with_rate_limit_until("Discord create_thread", || {
                self.client
                    .post(&url)
                    .header("Authorization", self.auth_header())
                    .header("Content-Type", "application/json")
                    .json(&body)
            }, deadline)
            .await
        {
            Ok(resp) => resp,
            // A rate-limit error from the admission gate or a received 429
            // proves this POST did not create a thread. Do not issue a
            // speculative GET that would turn an ordinary FIFO retry into an
            // irreversible ambiguous quarantine.
            Err(error @ GatewayError::RateLimited { .. }) => {
                return Err(DiscordThreadCreationFailure::Retryable(error));
            }
            Err(create_error) => {
                return self
                    .fetch_and_validate_message_thread(channel_id, message_id)
                    .await
                    .map_err(|recovery_error| {
                        DiscordThreadCreationFailure::Ambiguous(GatewayError::SendFailed(
                            format!(
                                "Discord create_thread failed: {create_error}; stable thread recovery failed: {recovery_error}"
                            ),
                        ))
                    });
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = match self
                .response_text_with_deadline(resp, deadline, "Discord create_thread")
                .await
            {
                Ok(text) => text,
                Err(body_error) => {
                    return self
                        .fetch_and_validate_message_thread(channel_id, message_id)
                        .await
                        .map_err(|recovery_error| {
                            DiscordThreadCreationFailure::Ambiguous(GatewayError::SendFailed(
                                format!(
                                    "Discord create_thread response body was unavailable: {body_error}; stable thread recovery failed: {recovery_error}"
                                ),
                            ))
                        });
                }
            };
            let error_code = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|body| body.get("code").and_then(serde_json::Value::as_i64));
            if status == reqwest::StatusCode::BAD_REQUEST && error_code == Some(160004) {
                return self
                    .fetch_and_validate_message_thread(channel_id, message_id)
                    .await
                    .map_err(DiscordThreadCreationFailure::Ambiguous);
            }
            let error = GatewayError::SendFailed(format!(
                "Discord create_thread API error ({status}): {text}"
            ));
            if status.is_server_error() {
                return match self
                    .fetch_and_validate_message_thread(channel_id, message_id)
                    .await
                {
                    Ok(thread) => Ok(thread),
                    Err(recovery_error) => {
                        Err(DiscordThreadCreationFailure::Ambiguous(GatewayError::SendFailed(
                            format!(
                                "{error}; stable thread recovery failed: {recovery_error}"
                            ),
                        )))
                    }
                };
            }
            return Err(
                if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                    DiscordThreadCreationFailure::Rejected(error)
                } else {
                    DiscordThreadCreationFailure::Ambiguous(error)
                },
            );
        }

        match self
            .response_json_with_deadline::<DiscordThread>(resp, deadline, "Discord create_thread")
            .await
            .and_then(|thread| Self::validate_message_thread(thread, channel_id, message_id))
        {
            Ok(thread) => Ok(thread),
            Err(decode_error) => self
                .fetch_and_validate_message_thread(channel_id, message_id)
                .await
                .map_err(|recovery_error| {
                    DiscordThreadCreationFailure::Ambiguous(GatewayError::SendFailed(format!(
                        "Discord created a message thread but its response was invalid: {decode_error}; stable thread recovery failed: {recovery_error}"
                    )))
                }),
        }
    }

    async fn fetch_and_validate_message_thread(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<DiscordThread, GatewayError> {
        let deadline = tokio::time::Instant::now() + DISCORD_REST_RATE_LIMIT_MAX_WAIT;
        let resp = self
            .send_rest_request_with_rate_limit_until(
                "Discord existing message thread lookup",
                || {
                    self.client
                        .get(self.api_url(&format!("channels/{message_id}")))
                        .header("Authorization", self.auth_header())
                },
                deadline,
            )
            .await
            .map_err(|error| {
                GatewayError::SendFailed(format!(
                    "Discord existing message thread lookup failed: {error}"
                ))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = self
                .response_text_with_deadline(
                    resp,
                    deadline,
                    "Discord existing message thread lookup",
                )
                .await?;
            return Err(GatewayError::SendFailed(format!(
                "Discord existing message thread lookup error ({status}): {text}"
            )));
        }
        let thread: DiscordThread = self
            .response_json_with_deadline(
                resp,
                deadline,
                "Discord existing message thread lookup",
            )
            .await?;
        Self::validate_message_thread(thread, channel_id, message_id)
    }

    fn validate_message_thread(
        thread: DiscordThread,
        channel_id: &str,
        message_id: &str,
    ) -> Result<DiscordThread, GatewayError> {
        let is_thread_type = matches!(thread.thread_type, Some(10 | 11 | 12));
        if thread.id != message_id
            || thread.parent_id.as_deref() != Some(channel_id)
            || !is_thread_type
        {
            return Err(GatewayError::SendFailed(
                "Discord message thread identity did not match its source message and parent"
                    .to_string(),
            ));
        }
        Ok(thread)
    }

    /// Enrich an accepted parent-channel message with its durable thread ID.
    pub async fn prepare_accepted_ingress(
        &self,
        incoming: &crate::gateway::IncomingMessage,
        is_reply: bool,
    ) -> Result<Option<String>, crate::gateway::AcceptedIngressPreparationError> {
        if !incoming.platform.eq_ignore_ascii_case("discord") {
            return Err(crate::gateway::AcceptedIngressPreparationError::Rejected(
                GatewayError::Platform(
                    "Discord ingress preparer received a different platform".to_string(),
                ),
            ));
        }
        if incoming.is_dm || incoming.thread_id.is_some() || is_reply {
            return Ok(None);
        }
        let channel_context = DiscordChannelContext::server(&incoming.chat_id);
        if !self
            .config
            .channel_controls
            .should_auto_thread(&channel_context)
        {
            return Ok(None);
        }
        let Some(message_id) = incoming.message_id.as_deref() else {
            warn!(
                channel_id = incoming.chat_id,
                "Accepted Discord message lacks an ID; continuing without auto-threading"
            );
            return Ok(None);
        };
        let accepted_state_permit = self
            .admit_accepted_state_commit()
            .map_err(crate::gateway::AcceptedIngressPreparationError::retryable)?;
        let thread = match self
            .create_thread_classified_with_permit(
                &incoming.chat_id,
                message_id,
                &discord_auto_thread_name(&incoming.text),
                None,
                &accepted_state_permit,
            )
            .await
        {
            Ok(thread) => thread,
            Err(DiscordThreadCreationFailure::Rejected(error)) => {
                warn!(
                    channel_id = incoming.chat_id,
                    message_id,
                    error = %error,
                    "Discord conclusively rejected auto-thread creation; continuing in the parent channel"
                );
                return Ok(None);
            }
            Err(DiscordThreadCreationFailure::Retryable(error)) => {
                // The creation POST is proven not to have been accepted. Keep
                // its classification intact so the ingress owner can preserve
                // its durable FIFO head for retry.
                return Err(
                    crate::gateway::AcceptedIngressPreparationError::retryable(error),
                );
            }
            Err(DiscordThreadCreationFailure::Ambiguous(error)) => {
                return Err(crate::gateway::AcceptedIngressPreparationError::Ambiguous(
                    GatewayError::SendFailed(format!(
                        "Discord auto-thread creation is ambiguous; refusing parent-channel fallback: {error}"
                    )),
                ));
            }
        };
        self.persist_thread_participation(&accepted_state_permit, &thread.id)
            .await
            .map_err(|error| {
                crate::gateway::AcceptedIngressPreparationError::Ambiguous(
                    Self::accepted_state_undurable(
                        vec![thread.id.clone()],
                        "thread participation",
                        error,
                    ),
                )
            })?;
        Ok(Some(thread.id))
    }

    /// Rename an existing Discord thread/channel.
    pub async fn rename_thread_channel(
        &self,
        thread_id: &str,
        title: &str,
    ) -> Result<(), GatewayError> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            return Err(GatewayError::SendFailed(
                "Discord thread rename requires a thread id".into(),
            ));
        }
        let title = title.trim();
        let title = if title.is_empty() { "Hermes" } else { title };
        let name = truncate_discord_utf16_with_suffix(title, 100, "...");
        let url = self.api_url(&format!("channels/{thread_id}"));
        let body = serde_json::json!({ "name": name });

        let resp = self
            .send_rest_request_with_rate_limit("Discord rename_thread", || {
                self.client
                    .patch(&url)
                    .header("Authorization", self.auth_header())
                    .header("Content-Type", "application/json")
                    .json(&body)
            })
            .await?;

        if !resp.status().is_success() {
            let text = discord_response_text_limited(resp).await;
            return Err(GatewayError::SendFailed(format!(
                "Discord rename_thread API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Gateway WebSocket helpers
    // -----------------------------------------------------------------------

    /// Build an IDENTIFY payload for the Discord Gateway.
    pub fn build_identify_payload(&self) -> GatewayPayload {
        DiscordGatewayPayloadBuilder::from_config(&self.config).identify()
    }

    /// Build a HEARTBEAT payload.
    pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
        DiscordGatewayPayloadBuilder::heartbeat(sequence)
    }

    /// Build a RESUME payload.
    pub fn build_resume_payload(&self, session_id: &str, seq: u64) -> GatewayPayload {
        DiscordGatewayPayloadBuilder::from_config(&self.config).resume(session_id, seq)
    }

    fn abort_tracker_writers(&self) {
        if let Ok(mut writer) = self.thread_participation_writer.lock() {
            if let Some(writer) = writer.take() {
                writer.abort();
            }
        }
        if let Ok(mut writer) = self.non_conversational_writer.lock() {
            if let Some(writer) = writer.take() {
                writer.abort();
            }
        }
    }
}

impl Drop for DiscordAdapter {
    fn drop(&mut self) {
        self.close_accepted_state_admission();
        self.base.mark_stopped();
        self.stop_liveness_probe();
        self.gateway_driver.cancel_and_abort_on_drop();
        self.abort_tracker_writers();
    }
}

async fn run_discord_liveness_probe_loop(
    client: Client,
    token: String,
    api_base_url: String,
    interval: std::time::Duration,
    threshold: u32,
    failed: Arc<AtomicBool>,
    stop: tokio::sync::watch::Receiver<bool>,
    global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
) {
    run_discord_liveness_probe_loop_with_timeout_and_global_rate_limit(
        client,
        token,
        api_base_url,
        interval,
        threshold,
        failed,
        stop,
        global_rate_limit,
        DISCORD_LIVENESS_PROBE_TIMEOUT,
    )
    .await;
}

async fn run_discord_liveness_probe_loop_with_timeout(
    client: Client,
    token: String,
    api_base_url: String,
    interval: std::time::Duration,
    threshold: u32,
    failed: Arc<AtomicBool>,
    stop: tokio::sync::watch::Receiver<bool>,
    probe_timeout: std::time::Duration,
) {
    run_discord_liveness_probe_loop_with_timeout_and_global_rate_limit(
        client,
        token,
        api_base_url,
        interval,
        threshold,
        failed,
        stop,
        Arc::new(DiscordGlobalRateLimitBarrier::default()),
        probe_timeout,
    )
    .await;
}

async fn run_discord_liveness_probe_loop_with_timeout_and_global_rate_limit(
    client: Client,
    token: String,
    api_base_url: String,
    interval: std::time::Duration,
    threshold: u32,
    failed: Arc<AtomicBool>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
    probe_timeout: std::time::Duration,
) {
    let mut failures = 0u32;
    let mut next_probe_delay = interval;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(next_probe_delay) => {}
            changed = stop.wait_for(|stopped| *stopped) => {
                let _ = changed;
                return;
            }
        }
        match probe_discord_rest_liveness_with_stop(
            &client,
            &token,
            &api_base_url,
            global_rate_limit.as_ref(),
            &mut stop,
            probe_timeout,
        )
        .await
        {
            Ok(()) => {
                failures = 0;
                next_probe_delay = interval;
            }
            Err(GatewayError::RateLimited { retry_after }) => {
                // A Discord 429 proves the REST endpoint is reachable. Keep
                // its exact delay locally (even for a non-global bucket)
                // before probing again, and never count it as unhealthy.
                failures = 0;
                next_probe_delay = retry_after
                    .map(|retry_after| std::cmp::max(interval, retry_after))
                    .unwrap_or(interval);
            }
            Err(err) => {
                if *stop.borrow() {
                    return;
                }
                next_probe_delay = interval;
                failures = failures.saturating_add(1);
                warn!(
                    failures,
                    threshold,
                    error = %err,
                    "Discord REST liveness probe failed"
                );
                if failures >= threshold {
                    failed.store(true, Ordering::SeqCst);
                    error!(
                        failures,
                        "Discord REST liveness threshold reached; adapter marked unhealthy for reconnect"
                    );
                    return;
                }
            }
        }
    }
}

async fn discord_response_bytes_limited(
    mut resp: reqwest::Response,
    limit_bytes: usize,
) -> Result<(Vec<u8>, bool), GatewayError> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|err| GatewayError::SendFailed(format!("Discord response read failed: {err}")))?
    {
        if body.len() + chunk.len() > limit_bytes {
            let remaining = limit_bytes.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

fn discord_http_status_error(status: reqwest::StatusCode, body: String) -> GatewayError {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|json| json.get("retry_after").and_then(serde_json::Value::as_f64))
            .and_then(discord_retry_after_duration);
        return GatewayError::RateLimited { retry_after };
    }
    if status.is_server_error() {
        return GatewayError::ConnectionFailed(format!(
            "Discord API returned HTTP {status}: {body}"
        ));
    }
    GatewayError::SendFailed(format!("Discord API returned HTTP {status}: {body}"))
}

/// Only a small set of Discord client failures proves that an embed/upload
/// POST was rejected before it could create a message. Timeout, rate-limit,
/// server, and transport failures remain ambiguous and must never trigger a
/// fallback that could duplicate a remote side effect.
fn discord_media_http_status_error(status: reqwest::StatusCode, body: String) -> GatewayError {
    if matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::UNAUTHORIZED
            | reqwest::StatusCode::FORBIDDEN
            | reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::PAYLOAD_TOO_LARGE
            | reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
            | reqwest::StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return GatewayError::MediaRemoteRejected {
            status: status.as_u16(),
        };
    }

    discord_http_status_error(status, body)
}

async fn discord_response_text_limited(resp: reqwest::Response) -> String {
    let Ok((body, truncated)) =
        discord_response_bytes_limited(resp, DISCORD_REST_ERROR_BODY_LIMIT_BYTES).await
    else {
        return String::new();
    };
    let mut text = String::from_utf8_lossy(&body).to_string();
    if truncated {
        text.push_str("... [truncated]");
    }
    text
}

async fn discord_response_json_limited<T>(resp: reqwest::Response) -> Result<T, GatewayError>
where
    T: serde::de::DeserializeOwned,
{
    let (body, truncated) =
        discord_response_bytes_limited(resp, DISCORD_REST_JSON_BODY_LIMIT_BYTES).await?;
    if truncated {
        return Err(GatewayError::SendFailed(format!(
            "Discord API JSON response exceeds {} bytes",
            DISCORD_REST_JSON_BODY_LIMIT_BYTES
        )));
    }
    serde_json::from_slice(&body)
        .map_err(|err| GatewayError::SendFailed(format!("Failed to parse Discord response: {err}")))
}

#[cfg(test)]
async fn probe_discord_rest_liveness(
    client: &Client,
    token: &str,
    api_base_url: &str,
) -> Result<(), GatewayError> {
    let (_stop_tx, mut stop) = tokio::sync::watch::channel(false);
    let global_rate_limit = DiscordGlobalRateLimitBarrier::default();
    probe_discord_rest_liveness_with_stop(
        client,
        token,
        api_base_url,
        &global_rate_limit,
        &mut stop,
        DISCORD_LIVENESS_PROBE_TIMEOUT,
    )
    .await
}

async fn probe_discord_rest_liveness_with_stop(
    client: &Client,
    token: &str,
    api_base_url: &str,
    global_rate_limit: &DiscordGlobalRateLimitBarrier,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    deadline: std::time::Duration,
) -> Result<(), GatewayError> {
    if *stop.borrow() {
        return Err(GatewayError::ConnectionFailed(
            "Discord liveness REST probe cancelled because the adapter is stopping".to_string(),
        ));
    }
    let deadline_at = tokio::time::Instant::now() + deadline;
    let url = format!("{}/users/@me", api_base_url.trim_end_matches('/'));
    let mut request_stop = stop.clone();
    let request = async {
        global_rate_limit
            .wait_until_open(deadline_at, &mut request_stop, "Discord liveness probe")
            .await?;
        let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(GatewayError::ConnectionFailed(
                "Discord liveness REST probe exceeded its deadline before request".to_string(),
            ));
        }
        let resp = client
            .get(url)
            .header("Authorization", format!("Bot {token}"))
            .timeout(remaining)
            .send()
            .await
            .map_err(|err| {
                GatewayError::ConnectionFailed(format!(
                    "Discord liveness REST request failed: {err}"
                ))
            })?;
        let status = resp.status();
        let header_retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<f64>().ok())
            .and_then(discord_retry_after_duration);
        let header_global = discord_rate_limit_is_global_header(resp.headers());
        let header_delay =
            header_retry_after.unwrap_or(std::time::Duration::from_secs(1));
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Classify a header-proven 429 before waiting on an untrusted body.
            // This keeps the liveness loop healthy and publishes global
            // admission immediately even if the peer never terminates JSON.
            if header_global {
                global_rate_limit.extend(header_delay);
            }
            let text = tokio::select! {
                body = discord_response_text_limited(resp) => body,
                _ = tokio::time::sleep_until(deadline_at) => {
                    return Err(GatewayError::RateLimited {
                        retry_after: Some(header_delay),
                    });
                }
                changed = request_stop.changed() => {
                    let _ = changed;
                    return Err(GatewayError::RateLimited {
                        retry_after: Some(header_delay),
                    });
                }
            };
            let (retry_after, body_global) =
                discord_rate_limit_delay(&text, header_retry_after);
            if header_global || body_global {
                global_rate_limit.extend(retry_after);
            }
            return Err(GatewayError::RateLimited {
                retry_after: Some(retry_after),
            });
        }

        // Drain every non-429 body under the same probe deadline. Unlike a
        // 429, a stalled success/error body cannot prove provider reachability.
        let text = tokio::select! {
            body = discord_response_text_limited(resp) => body,
            _ = tokio::time::sleep_until(deadline_at) => {
                return Err(GatewayError::ConnectionFailed(format!(
                    "Discord liveness REST probe exceeded {} ms",
                    deadline.as_millis()
                )));
            }
            changed = request_stop.changed() => {
                let _ = changed;
                return Err(GatewayError::ConnectionFailed(
                    "Discord liveness REST probe cancelled because the adapter is stopping".to_string(),
                ));
            }
        };
        if status.is_success() {
            Ok(())
        } else {
            Err(GatewayError::ConnectionFailed(format!(
                "Discord liveness REST status {status}: {text}"
            )))
        }
    };
    tokio::select! {
        result = request => result,
        changed = stop.wait_for(|stopped| *stopped) => {
            let _ = changed;
            Err(GatewayError::ConnectionFailed(
                "Discord liveness REST probe cancelled because the adapter is stopping".to_string(),
            ))
        }
    }
}
