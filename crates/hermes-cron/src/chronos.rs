//! Chronos managed-cron provider.
//!
//! Chronos is the NAS-mediated cron provider used for hosted agents that can
//! scale to zero. The agent computes each job's next fire time, asks NAS to arm
//! one external one-shot, and verifies the NAS callback before running the job.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::SecondsFormat;
use hermes_config::managed_gateway::peek_nous_access_token;
use hermes_config::read_nous_access_token;
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::job::{CronJob, JobStatus};
use crate::scheduler::{CronError, ManagedCronProvider};

const DEFAULT_TIMEOUT_SECONDS: u64 = 15;
const DEFAULT_LEEWAY_SECONDS: u64 = 30;
const PROVISION_PATH: &str = "/api/agent-cron/provision";
const CANCEL_PATH: &str = "/api/agent-cron/cancel";
const LIST_PATH: &str = "/api/agent-cron/list";
const FIRE_PURPOSE: &str = "cron_fire";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChronosConfig {
    pub provider: String,
    pub portal_url: String,
    pub callback_url: String,
    pub expected_audience: String,
    pub nas_jwks_url: String,
    pub token_leeway_seconds: u64,
    pub request_timeout_seconds: u64,
}

impl Default for ChronosConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            portal_url: String::new(),
            callback_url: String::new(),
            expected_audience: String::new(),
            nas_jwks_url: String::new(),
            token_leeway_seconds: DEFAULT_LEEWAY_SECONDS,
            request_timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }
}

impl ChronosConfig {
    pub fn load() -> Self {
        let yaml = load_config_yaml();
        let provider = env_or_yaml("HERMES_CRON_PROVIDER", &yaml, &["cron", "provider"]);
        let portal_url = env_or_yaml(
            "HERMES_CHRONOS_PORTAL_URL",
            &yaml,
            &["cron", "chronos", "portal_url"],
        );
        let callback_url = env_or_yaml(
            "HERMES_CHRONOS_CALLBACK_URL",
            &yaml,
            &["cron", "chronos", "callback_url"],
        );
        let expected_audience = env_or_yaml(
            "HERMES_CHRONOS_EXPECTED_AUDIENCE",
            &yaml,
            &["cron", "chronos", "expected_audience"],
        );
        let nas_jwks_url = env_or_yaml(
            "HERMES_CHRONOS_NAS_JWKS_URL",
            &yaml,
            &["cron", "chronos", "nas_jwks_url"],
        );

        Self {
            provider,
            portal_url,
            callback_url,
            expected_audience,
            nas_jwks_url,
            token_leeway_seconds: env_or_yaml_u64(
                "HERMES_CHRONOS_TOKEN_LEEWAY_SECONDS",
                &yaml,
                &["cron", "chronos", "token_leeway_seconds"],
                DEFAULT_LEEWAY_SECONDS,
            ),
            request_timeout_seconds: env_or_yaml_u64(
                "HERMES_CHRONOS_REQUEST_TIMEOUT_SECONDS",
                &yaml,
                &["cron", "chronos", "request_timeout_seconds"],
                DEFAULT_TIMEOUT_SECONDS,
            ),
        }
    }

    pub fn provider_enabled(&self) -> bool {
        self.provider.trim().eq_ignore_ascii_case("chronos")
    }

    pub fn can_provision(&self) -> bool {
        self.provider_enabled()
            && !self.portal_url.trim().is_empty()
            && !self.callback_url.trim().is_empty()
            && peek_nous_access_token().is_some()
    }

    pub fn can_verify_fire(&self) -> bool {
        self.provider_enabled()
            && !self.expected_audience.trim().is_empty()
            && !self.nas_jwks_url.trim().is_empty()
    }
}

fn load_config_yaml() -> Option<Value> {
    let bytes = std::fs::read(hermes_config::config_path()).ok()?;
    serde_yaml::from_slice::<Value>(&bytes).ok()
}

fn yaml_string(root: &Option<Value>, path: &[&str]) -> Option<String> {
    let mut cursor = root.as_ref()?;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    match cursor {
        Value::String(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn env_or_yaml(env: &str, yaml: &Option<Value>, path: &[&str]) -> String {
    std::env::var(env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| yaml_string(yaml, path))
        .unwrap_or_default()
}

fn env_or_yaml_u64(env: &str, yaml: &Option<Value>, path: &[&str], default: u64) -> u64 {
    env_or_yaml(env, yaml, path)
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[derive(Debug, Clone)]
pub struct ChronosNasCronProvider {
    config: ChronosConfig,
    client: reqwest::Client,
}

impl ChronosNasCronProvider {
    pub fn from_environment() -> Self {
        Self::new(ChronosConfig::load())
    }

    pub fn new(config: ChronosConfig) -> Self {
        let timeout = Duration::from_secs(config.request_timeout_seconds.max(1));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    pub fn config(&self) -> &ChronosConfig {
        &self.config
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.config.portal_url.trim_end_matches('/'), path)
    }

    fn access_token(&self) -> Result<String, CronError> {
        read_nous_access_token(None).ok_or_else(|| {
            CronError::Scheduler(
                "Chronos requires a Nous Portal access token; run `hermes auth login nous`"
                    .to_string(),
            )
        })
    }

    async fn provision(
        &self,
        job_id: &str,
        fire_at: &str,
        dedup_key: &str,
    ) -> Result<(), CronError> {
        let token = self.access_token()?;
        let body = serde_json::json!({
            "job_id": job_id,
            "fire_at": fire_at,
            "agent_callback_url": self.config.callback_url,
            "dedup_key": dedup_key,
        });
        self.client
            .post(self.endpoint(PROVISION_PATH))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos provision failed: {err}")))?
            .error_for_status()
            .map_err(|err| CronError::Scheduler(format!("Chronos provision rejected: {err}")))?;
        Ok(())
    }

    async fn cancel_one(&self, job_id: &str) -> Result<(), CronError> {
        let token = self.access_token()?;
        self.client
            .post(self.endpoint(CANCEL_PATH))
            .bearer_auth(token)
            .json(&serde_json::json!({ "job_id": job_id }))
            .send()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos cancel failed: {err}")))?
            .error_for_status()
            .map_err(|err| CronError::Scheduler(format!("Chronos cancel rejected: {err}")))?;
        Ok(())
    }

    async fn list_armed(&self) -> Result<BTreeMap<String, String>, CronError> {
        let token = self.access_token()?;
        let response: ChronosListResponse = self
            .client
            .get(self.endpoint(LIST_PATH))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos list failed: {err}")))?
            .error_for_status()
            .map_err(|err| CronError::Scheduler(format!("Chronos list rejected: {err}")))?
            .json()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos list parse failed: {err}")))?;
        Ok(response
            .armed
            .into_iter()
            .filter_map(|item| item.job_id.map(|id| (id, item.fire_at.unwrap_or_default())))
            .collect())
    }
}

#[async_trait]
impl ManagedCronProvider for ChronosNasCronProvider {
    fn name(&self) -> &'static str {
        "chronos"
    }

    fn is_available(&self) -> bool {
        self.config.can_provision()
    }

    async fn reconcile(&self, jobs: Vec<CronJob>) -> Result<(), CronError> {
        if !self.is_available() {
            return Ok(());
        }

        let desired = jobs
            .into_iter()
            .filter(|job| job.status == JobStatus::Active)
            .filter_map(|job| {
                let fire_at = job
                    .next_run
                    .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, false))?;
                Some((job.id, fire_at))
            })
            .collect::<BTreeMap<_, _>>();

        let observed = match self.list_armed().await {
            Ok(observed) => observed,
            Err(err) => {
                tracing::debug!(
                    "Chronos list failed during reconcile; re-arming desired jobs: {err}"
                );
                BTreeMap::new()
            }
        };

        for (job_id, fire_at) in &desired {
            if observed.get(job_id) != Some(fire_at) {
                let dedup_key = format!("{job_id}:{fire_at}");
                self.provision(job_id, fire_at, &dedup_key).await?;
            }
        }

        for job_id in observed.keys() {
            if !desired.contains_key(job_id) {
                self.cancel_one(job_id).await?;
            }
        }

        Ok(())
    }

    async fn cancel(&self, job_id: &str) -> Result<(), CronError> {
        if !self.is_available() {
            return Ok(());
        }
        self.cancel_one(job_id).await
    }
}

#[derive(Debug, Deserialize)]
struct ChronosListResponse {
    #[serde(default)]
    armed: Vec<ChronosArmedJob>,
}

#[derive(Debug, Deserialize)]
struct ChronosArmedJob {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    fire_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChronosFireClaims {
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub aud: Option<Value>,
    #[serde(default)]
    pub exp: Option<u64>,
    #[serde(default)]
    pub nbf: Option<u64>,
}

pub async fn verify_nas_fire_token(
    token: &str,
    config: &ChronosConfig,
) -> Result<ChronosFireClaims, CronError> {
    if token.trim().is_empty() {
        return Err(CronError::Scheduler(
            "missing Chronos bearer token".to_string(),
        ));
    }
    if !config.can_verify_fire() {
        return Err(CronError::Scheduler(
            "Chronos fire verification is not configured".to_string(),
        ));
    }

    let header = decode_header(token)
        .map_err(|err| CronError::Scheduler(format!("Chronos token header invalid: {err}")))?;
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    ) {
        return Err(CronError::Scheduler(
            "Chronos token uses an unsupported signing algorithm".to_string(),
        ));
    }

    let key = decoding_key_for_token(&header.kid, &config.nas_jwks_url).await?;
    let mut validation = Validation::new(header.alg);
    validation.leeway = config.token_leeway_seconds;
    validation.validate_nbf = true;
    validation.set_audience(&[config.expected_audience.as_str()]);
    validation.required_spec_claims.insert("aud".to_string());
    validation.required_spec_claims.insert("exp".to_string());
    if !config.portal_url.trim().is_empty() {
        validation.set_issuer(&[config.portal_url.trim_end_matches('/')]);
        validation.required_spec_claims.insert("iss".to_string());
    }

    let data = decode::<ChronosFireClaims>(token, &key, &validation)
        .map_err(|err| CronError::Scheduler(format!("Chronos token rejected: {err}")))?;
    if data.claims.purpose.as_deref() != Some(FIRE_PURPOSE) {
        return Err(CronError::Scheduler(
            "Chronos token missing cron_fire purpose".to_string(),
        ));
    }

    Ok(data.claims)
}

async fn decoding_key_for_token(
    kid: &Option<String>,
    jwks_or_key: &str,
) -> Result<DecodingKey, CronError> {
    let source = jwks_or_key.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        let set: JwkSet = reqwest::Client::new()
            .get(source)
            .send()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos JWKS fetch failed: {err}")))?
            .error_for_status()
            .map_err(|err| CronError::Scheduler(format!("Chronos JWKS rejected: {err}")))?
            .json()
            .await
            .map_err(|err| CronError::Scheduler(format!("Chronos JWKS parse failed: {err}")))?;
        let jwk = select_jwk(&set, kid)?;
        return DecodingKey::from_jwk(jwk)
            .map_err(|err| CronError::Scheduler(format!("Chronos JWKS key invalid: {err}")));
    }

    if source.starts_with('{') {
        let value: Value = serde_json::from_str(source)
            .map_err(|err| CronError::Scheduler(format!("Chronos inline JWKS invalid: {err}")))?;
        if value.get("keys").is_some() {
            let set: JwkSet = serde_json::from_value(value).map_err(|err| {
                CronError::Scheduler(format!("Chronos inline JWKS parse failed: {err}"))
            })?;
            let jwk = select_jwk(&set, kid)?;
            return DecodingKey::from_jwk(jwk)
                .map_err(|err| CronError::Scheduler(format!("Chronos JWKS key invalid: {err}")));
        }
        let jwk: Jwk = serde_json::from_value(value).map_err(|err| {
            CronError::Scheduler(format!("Chronos inline JWK parse failed: {err}"))
        })?;
        reject_symmetric_jwk(&jwk)?;
        return DecodingKey::from_jwk(&jwk)
            .map_err(|err| CronError::Scheduler(format!("Chronos JWK key invalid: {err}")));
    }

    DecodingKey::from_rsa_pem(source.as_bytes())
        .or_else(|_| DecodingKey::from_ec_pem(source.as_bytes()))
        .map_err(|err| CronError::Scheduler(format!("Chronos PEM key invalid: {err}")))
}

fn select_jwk<'a>(set: &'a JwkSet, kid: &Option<String>) -> Result<&'a Jwk, CronError> {
    let jwk = if let Some(kid) = kid {
        set.find(kid).ok_or_else(|| {
            CronError::Scheduler(format!("Chronos JWKS did not contain kid {kid}"))
        })?
    } else if set.keys.len() == 1 {
        &set.keys[0]
    } else {
        return Err(CronError::Scheduler(
            "Chronos token has no kid and JWKS has multiple keys".to_string(),
        ));
    };
    reject_symmetric_jwk(jwk)?;
    Ok(jwk)
}

fn reject_symmetric_jwk(jwk: &Jwk) -> Result<(), CronError> {
    if matches!(jwk.algorithm, AlgorithmParameters::OctetKey(_)) {
        return Err(CronError::Scheduler(
            "Chronos fire tokens must use asymmetric signing keys".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    #[test]
    fn config_reads_upstream_chronos_yaml_shape() {
        let root = Some(json!({
            "cron": {
                "provider": "chronos",
                "chronos": {
                    "portal_url": "https://portal.example",
                    "callback_url": "https://agent.example",
                    "expected_audience": "agent:abc",
                    "nas_jwks_url": "https://portal.example/.well-known/jwks.json",
                    "token_leeway_seconds": 42
                }
            }
        }));
        assert_eq!(
            yaml_string(&root, &["cron", "chronos", "portal_url"]).as_deref(),
            Some("https://portal.example")
        );
        assert_eq!(
            env_or_yaml_u64(
                "HERMES_TEST_CHRONOS_NONE",
                &root,
                &["cron", "chronos", "token_leeway_seconds"],
                30
            ),
            42
        );
    }

    #[tokio::test]
    async fn fire_verifier_rejects_symmetric_jwks() {
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "aud": "agent:abc",
                "iss": "https://portal.example",
                "exp": 4_102_444_800_u64,
                "purpose": FIRE_PURPOSE
            }),
            &EncodingKey::from_secret(b"secret"),
        )
        .expect("encode test token");
        let key = base64_url("secret");
        let config = ChronosConfig {
            provider: "chronos".to_string(),
            portal_url: "https://portal.example".to_string(),
            expected_audience: "agent:abc".to_string(),
            nas_jwks_url: json!({
                "keys": [{
                    "kty": "oct",
                    "alg": "HS256",
                    "kid": "test",
                    "k": key
                }]
            })
            .to_string(),
            ..ChronosConfig::default()
        };
        let err = verify_nas_fire_token(&token, &config).await.unwrap_err();
        assert!(err.to_string().contains("unsupported signing algorithm"));
    }

    fn base64_url(raw: &str) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        URL_SAFE_NO_PAD.encode(raw.as_bytes())
    }
}
