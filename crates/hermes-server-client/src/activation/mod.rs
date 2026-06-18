//! Device activation reporting (`POST /device/activate`).

mod fingerprint;
mod geoip;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

pub use fingerprint::{DeviceFingerprint, build_activate_request, collect_fingerprint};
pub use geoip::{GeoIpInfo, resolve_geo_ip};

use crate::error::ServerClientError;
use crate::flowy::FlowyApiClient;
use crate::paths::device_state_path;
use crate::session::ServerSession;

/// Reuse geo lookups for this long to avoid hammering external APIs on repeated logins.
const GEO_CACHE_TTL_MS: i64 = 86_400_000; // 24 hours

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GeoIpCacheEntry {
    fetched_at_ms: i64,
    #[serde(default)]
    info: GeoIpInfo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeviceStateFile {
    #[serde(default)]
    sn: String,
    /// user_id → app versions already reported successfully for that user.
    #[serde(default)]
    activations_by_user: HashMap<String, HashSet<String>>,
    /// Last public IP successfully reported per user (triggers re-activation when changed).
    #[serde(default)]
    last_reported_ip_by_user: HashMap<String, String>,
    #[serde(default)]
    geo_cache: Option<GeoIpCacheEntry>,
    /// Legacy single-user flag (read-only migration aid).
    #[serde(default, skip_serializing)]
    activation_uploaded: bool,
    #[serde(default, skip_serializing)]
    activation_uploaded_app_version: String,
}

pub struct DeviceActivation {
    state_path: std::path::PathBuf,
}

impl DeviceActivation {
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        Self {
            state_path: device_state_path(hermes_home.as_ref()),
        }
    }

    /// Report activation once per `(user_id, app_version)`; re-report when public IP changes.
    pub async fn try_activate_for_user(
        &self,
        api: &FlowyApiClient,
        session: &ServerSession,
        user_id: i64,
    ) -> Result<bool, ServerClientError> {
        let app_version = env!("CARGO_PKG_VERSION");
        let mut state = self.load_state().await?;

        let persisted_sn = if state.sn.is_empty() {
            None
        } else {
            Some(state.sn.as_str())
        };
        let fingerprint = collect_fingerprint(persisted_sn)?;
        if state.sn.is_empty() {
            state.sn = fingerprint.sn.clone();
        }

        // When already activated for this version, bypass geo cache so we can detect IP changes.
        let force_fresh_geo = already_activated(&state, user_id, app_version);
        let geo = self.resolve_geo(&mut state, force_fresh_geo).await;
        let current_ip = geo
            .as_ref()
            .map(|g| g.public_ip.as_str())
            .unwrap_or_default();

        if should_skip_activation(&state, user_id, app_version, current_ip) {
            debug!(
                user_id,
                app_version,
                current_ip,
                "device activation up to date for this user, version, and IP"
            );
            self.save_state(&state).await?;
            return Ok(false);
        }

        if force_fresh_geo {
            debug!(
                user_id,
                app_version,
                current_ip,
                "public IP changed — re-reporting device activation"
            );
        }

        let mut request =
            build_activate_request(api.config().channel.as_str(), &fingerprint, geo.as_ref());
        request.sn = state.sn.clone();

        match api.device_activate(session, &request).await {
            Ok(()) => {
                record_activation(&mut state, user_id, app_version, current_ip);
                self.save_state(&state).await?;
                Ok(true)
            }
            Err(err) => {
                warn!(error = %err, user_id, "device activation upload failed");
                self.save_state(&state).await?;
                Err(err)
            }
        }
    }

    async fn resolve_geo(&self, state: &mut DeviceStateFile, force_refresh: bool) -> Option<GeoIpInfo> {
        if !force_refresh
            && let Some(cache) = &state.geo_cache
            && geo_cache_fresh(cache)
        {
            debug!("using cached geoip (age_ms={})", geo_cache_age_ms(cache));
            return Some(cache.info.clone());
        }

        let info = resolve_geo_ip().await?;
        state.geo_cache = Some(GeoIpCacheEntry {
            fetched_at_ms: Utc::now().timestamp_millis(),
            info: info.clone(),
        });
        Some(info)
    }

    async fn load_state(&self) -> Result<DeviceStateFile, ServerClientError> {
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        if !tokio::fs::try_exists(&self.state_path)
            .await
            .unwrap_or(false)
        {
            return Ok(DeviceStateFile::default());
        }
        let raw = tokio::fs::read_to_string(&self.state_path)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))?;
        let mut state: DeviceStateFile =
            serde_json::from_str(&raw).map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        migrate_legacy_activation(&mut state);
        Ok(state)
    }

    async fn save_state(&self, state: &DeviceStateFile) -> Result<(), ServerClientError> {
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        let raw = serde_json::to_string_pretty(state)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        tokio::fs::write(&self.state_path, raw)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))
    }
}

fn already_activated(state: &DeviceStateFile, user_id: i64, app_version: &str) -> bool {
    state
        .activations_by_user
        .get(&user_id.to_string())
        .is_some_and(|versions| versions.contains(app_version))
}

fn should_skip_activation(
    state: &DeviceStateFile,
    user_id: i64,
    app_version: &str,
    current_ip: &str,
) -> bool {
    if !already_activated(state, user_id, app_version) {
        return false;
    }
    if current_ip.is_empty() {
        // Geo unavailable — avoid hammering the server on every login.
        return true;
    }
    state
        .last_reported_ip_by_user
        .get(&user_id.to_string())
        .is_some_and(|last| last == current_ip)
}

fn record_activation(
    state: &mut DeviceStateFile,
    user_id: i64,
    app_version: &str,
    public_ip: &str,
) {
    state
        .activations_by_user
        .entry(user_id.to_string())
        .or_default()
        .insert(app_version.to_string());
    if !public_ip.is_empty() {
        state
            .last_reported_ip_by_user
            .insert(user_id.to_string(), public_ip.to_string());
    }
}

fn geo_cache_fresh(cache: &GeoIpCacheEntry) -> bool {
    geo_cache_age_ms(cache) < GEO_CACHE_TTL_MS
}

fn geo_cache_age_ms(cache: &GeoIpCacheEntry) -> i64 {
    Utc::now().timestamp_millis().saturating_sub(cache.fetched_at_ms)
}

/// Migrate pre-user-tracking activation flags into a synthetic legacy user bucket.
fn migrate_legacy_activation(state: &mut DeviceStateFile) {
    if state.activations_by_user.is_empty()
        && state.activation_uploaded
        && !state.activation_uploaded_app_version.is_empty()
    {
        state
            .activations_by_user
            .entry("_legacy_device".to_string())
            .or_default()
            .insert(state.activation_uploaded_app_version.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_version_dedup() {
        let mut state = DeviceStateFile::default();
        assert!(!already_activated(&state, 42, "0.16.0"));
        record_activation(&mut state, 42, "0.16.0", "203.0.113.1");
        assert!(already_activated(&state, 42, "0.16.0"));
        assert!(!already_activated(&state, 43, "0.16.0"));
        assert!(!already_activated(&state, 42, "0.17.0"));
    }

    #[test]
    fn skip_when_same_user_version_and_ip() {
        let mut state = DeviceStateFile::default();
        record_activation(&mut state, 42, "0.16.0", "203.0.113.1");
        assert!(should_skip_activation(&state, 42, "0.16.0", "203.0.113.1"));
    }

    #[test]
    fn reactivate_when_ip_changes() {
        let mut state = DeviceStateFile::default();
        record_activation(&mut state, 42, "0.16.0", "203.0.113.1");
        assert!(!should_skip_activation(&state, 42, "0.16.0", "203.0.113.99"));
    }

    #[test]
    fn first_activation_never_skipped_with_ip() {
        let state = DeviceStateFile::default();
        assert!(!should_skip_activation(&state, 42, "0.16.0", "203.0.113.1"));
    }

    #[test]
    fn skip_repeat_when_geo_unavailable_after_activation() {
        let mut state = DeviceStateFile::default();
        record_activation(&mut state, 42, "0.16.0", "203.0.113.1");
        assert!(should_skip_activation(&state, 42, "0.16.0", ""));
    }

    #[test]
    fn legacy_activation_migrates_to_bucket() {
        let mut state = DeviceStateFile {
            activation_uploaded: true,
            activation_uploaded_app_version: "0.15.0".into(),
            ..Default::default()
        };
        migrate_legacy_activation(&mut state);
        assert!(state
            .activations_by_user
            .get("_legacy_device")
            .is_some_and(|v| v.contains("0.15.0")));
    }

    #[test]
    fn geo_cache_ttl() {
        let stale = GeoIpCacheEntry {
            fetched_at_ms: Utc::now().timestamp_millis() - GEO_CACHE_TTL_MS - 1,
            info: GeoIpInfo::default(),
        };
        assert!(!geo_cache_fresh(&stale));
        let fresh = GeoIpCacheEntry {
            fetched_at_ms: Utc::now().timestamp_millis(),
            info: GeoIpInfo::default(),
        };
        assert!(geo_cache_fresh(&fresh));
    }
}
