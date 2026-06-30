//! Relay connector protocol contracts.
//!
//! Hermes Ultra does not register the upstream experimental relay connector as a
//! default platform adapter, but the gateway crate owns the stable wire
//! contracts we need to preserve when/if a transport is enabled.

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

pub const RELAY_UNAUTHORIZED_CLOSE_CODE: u16 = 4401;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayCloseDisposition {
    Retryable,
    Disabled,
}

/// Connector-forwarded passthrough-plane request (`passthrough_forward`).
///
/// The connector edge already verified provider signatures and stripped shared
/// identity credentials before forwarding this frame over the authenticated
/// outbound relay socket. `body` is decoded back to the exact byte payload carried
/// by the connector's `bodyB64` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayPassthroughForward {
    pub platform: String,
    pub bot_id: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub buffer_id: Option<String>,
}

impl RelayPassthroughForward {
    pub fn body_b64(&self) -> String {
        general_purpose::STANDARD.encode(&self.body)
    }
}

/// Decode a relay `passthrough_forward` frame.
///
/// Malformed base64 is tolerated as an empty body so a bad forwarded request
/// cannot kill the relay reader loop.
pub fn relay_passthrough_forward_from_frame(frame: &Value) -> Option<RelayPassthroughForward> {
    if frame.get("type").and_then(Value::as_str) != Some("passthrough_forward") {
        return None;
    }
    let raw = frame.get("forward")?.as_object()?;
    let body = raw
        .get("bodyB64")
        .and_then(Value::as_str)
        .and_then(|body| general_purpose::STANDARD.decode(body).ok())
        .unwrap_or_default();
    let headers = raw
        .get("headers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let pair = item.as_array()?;
                    if pair.len() != 2 {
                        return None;
                    }
                    Some((
                        pair[0].as_str().unwrap_or_default().to_string(),
                        pair[1].as_str().unwrap_or_default().to_string(),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(RelayPassthroughForward {
        platform: raw
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        bot_id: raw
            .get("botId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        method: raw
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        path: raw
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        headers,
        body,
        buffer_id: frame
            .get("bufferId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    })
}

pub fn relay_going_idle_frame() -> Value {
    json!({"type": "going_idle"})
}

pub fn relay_inbound_ack_frame(buffer_id: &str) -> Option<Value> {
    let buffer_id = buffer_id.trim();
    (!buffer_id.is_empty()).then(|| json!({"type": "inbound_ack", "bufferId": buffer_id}))
}

/// Resolve the platform-neutral relay scope discriminator.
///
/// `scope_id` is canonical; `guild_id` is a deprecated compatibility alias
/// retained during the cross-repo relay wire migration.
pub fn relay_scope_id_from_source(source: &Value) -> Option<String> {
    source
        .get("scope_id")
        .or_else(|| source.get("guild_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Dual-write canonical `scope_id` and deprecated `guild_id` alias.
pub fn relay_dual_write_scope_id(source: &mut Value, scope_id: &str) -> bool {
    let scope_id = scope_id.trim();
    if scope_id.is_empty() {
        return false;
    }
    let Some(map) = source.as_object_mut() else {
        return false;
    };
    map.insert("scope_id".to_string(), Value::String(scope_id.to_string()));
    map.insert("guild_id".to_string(), Value::String(scope_id.to_string()));
    true
}

/// Classify relay socket close events.
///
/// A connector `4401` before the first successful descriptor/handshake remains
/// retryable because provisioning may still be racing. The same close code after
/// a successful handshake is a terminal opt-out/credential-revocation signal.
pub fn relay_close_disposition(
    close_code: Option<u16>,
    handshake_succeeded: bool,
) -> RelayCloseDisposition {
    if close_code == Some(RELAY_UNAUTHORIZED_CLOSE_CODE) && handshake_succeeded {
        RelayCloseDisposition::Disabled
    } else {
        RelayCloseDisposition::Retryable
    }
}

pub fn relay_platform_state_for_close(
    close_code: Option<u16>,
    handshake_succeeded: bool,
) -> &'static str {
    match relay_close_disposition(close_code, handshake_succeeded) {
        RelayCloseDisposition::Disabled => "disabled",
        RelayCloseDisposition::Retryable => "retrying",
    }
}

/// Convert a relay dial URL to the management-plane `/relay/policy` URL.
pub fn relay_policy_url(relay_url: &str) -> Option<String> {
    let mut url = Url::parse(relay_url.trim().trim_end_matches('/')).ok()?;
    let scheme = match url.scheme() {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}/policy"));
    Some(url.to_string())
}

/// Wake URL precedence: managed/container env first, then config.yaml.
pub fn relay_wake_url_from_sources(
    env_value: Option<&str>,
    config_value: Option<&str>,
) -> Option<String> {
    env_value
        .and_then(normalize_relay_urlish)
        .or_else(|| config_value.and_then(normalize_relay_urlish))
}

fn normalize_relay_urlish(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    (!value.is_empty()).then(|| value.to_string())
}

/// Connector-side relevance policy projected from platform mention controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayRelevancePolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_address: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free_response_scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_other_bots: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn relay_relevance_policy<I, S>(
    require_mention: Option<bool>,
    free_response_scopes: I,
    allow_bots: Option<&str>,
) -> Option<RelayRelevancePolicy>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let free_response_scopes = free_response_scopes
        .into_iter()
        .flat_map(|item| {
            item.as_ref()
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let allow_other_bots = allow_bots
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .is_some_and(|value| matches!(value.as_str(), "mentions" | "all" | "true" | "1" | "yes"));
    if require_mention.is_none() && free_response_scopes.is_empty() && !allow_other_bots {
        return None;
    }
    Some(RelayRelevancePolicy {
        require_address: require_mention,
        free_response_scopes,
        allow_other_bots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_forward_decodes_exact_body_and_headers() {
        let body = b"{\"type\":2,\"data\":{\"name\":\"ship\"}}";
        let frame = json!({
            "type": "passthrough_forward",
            "bufferId": "buf-1",
            "forward": {
                "platform": "discord",
                "botId": "bot-7",
                "method": "POST",
                "path": "/interactions",
                "headers": [["content-type", "application/json"], ["x-extra", "1"]],
                "bodyB64": general_purpose::STANDARD.encode(body),
            }
        });
        let forward = relay_passthrough_forward_from_frame(&frame).expect("forward");
        assert_eq!(forward.platform, "discord");
        assert_eq!(forward.bot_id, "bot-7");
        assert_eq!(forward.body, body);
        assert_eq!(forward.body_b64(), general_purpose::STANDARD.encode(body));
        assert_eq!(forward.buffer_id.as_deref(), Some("buf-1"));
        assert_eq!(
            forward.headers,
            vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("x-extra".to_string(), "1".to_string())
            ]
        );
    }

    #[test]
    fn passthrough_forward_tolerates_malformed_body() {
        let frame = json!({
            "type": "passthrough_forward",
            "forward": {"platform": "discord", "bodyB64": "not-base64!!!"}
        });
        let forward = relay_passthrough_forward_from_frame(&frame).expect("forward");
        assert!(forward.body.is_empty());
    }

    #[test]
    fn relay_control_frames_match_connector_contract() {
        assert_eq!(relay_going_idle_frame(), json!({"type": "going_idle"}));
        assert_eq!(
            relay_inbound_ack_frame(" buf-9 ").unwrap(),
            json!({"type": "inbound_ack", "bufferId": "buf-9"})
        );
        assert!(relay_inbound_ack_frame("   ").is_none());
    }

    #[test]
    fn relay_scope_id_prefers_canonical_and_falls_back_to_guild_alias() {
        assert_eq!(
            relay_scope_id_from_source(&json!({"scope_id": "S1", "guild_id": "G1"})).as_deref(),
            Some("S1")
        );
        assert_eq!(
            relay_scope_id_from_source(&json!({"guild_id": "G1"})).as_deref(),
            Some("G1")
        );
        assert!(relay_scope_id_from_source(&json!({"scope_id": "  "})).is_none());
    }

    #[test]
    fn relay_scope_id_dual_writes_canonical_and_legacy_keys() {
        let mut source = json!({"platform": "discord", "chat_id": "C1"});
        assert!(relay_dual_write_scope_id(&mut source, "S1"));
        assert_eq!(source["scope_id"], "S1");
        assert_eq!(source["guild_id"], "S1");
        assert!(!relay_dual_write_scope_id(&mut source, " "));
        let mut non_object = json!(null);
        assert!(!relay_dual_write_scope_id(&mut non_object, "S1"));
    }

    #[test]
    fn terminal_4401_after_handshake_maps_to_disabled_not_retrying() {
        assert_eq!(
            relay_close_disposition(Some(RELAY_UNAUTHORIZED_CLOSE_CODE), true),
            RelayCloseDisposition::Disabled
        );
        assert_eq!(
            relay_platform_state_for_close(Some(RELAY_UNAUTHORIZED_CLOSE_CODE), true),
            "disabled"
        );
        assert_eq!(
            relay_close_disposition(Some(RELAY_UNAUTHORIZED_CLOSE_CODE), false),
            RelayCloseDisposition::Retryable
        );
        assert_eq!(relay_platform_state_for_close(Some(1006), true), "retrying");
    }

    #[test]
    fn relay_policy_url_maps_ws_to_http_management_plane() {
        assert_eq!(
            relay_policy_url("wss://relay.example.test/relay/").as_deref(),
            Some("https://relay.example.test/relay/policy")
        );
        assert_eq!(
            relay_policy_url("ws://127.0.0.1:8080/relay").as_deref(),
            Some("http://127.0.0.1:8080/relay/policy")
        );
        assert!(relay_policy_url("file:///tmp/relay").is_none());
    }

    #[test]
    fn wake_url_prefers_env_and_trims_trailing_slash() {
        assert_eq!(
            relay_wake_url_from_sources(
                Some(" https://wake.example.test/ "),
                Some("https://config.example.test/")
            )
            .as_deref(),
            Some("https://wake.example.test")
        );
        assert_eq!(
            relay_wake_url_from_sources(Some(" "), Some("https://config.example.test/")).as_deref(),
            Some("https://config.example.test")
        );
        assert!(relay_wake_url_from_sources(Some(" "), Some(" ")).is_none());
    }

    #[test]
    fn relevance_policy_projects_platform_knobs() {
        assert!(relay_relevance_policy(None, Vec::<String>::new(), None).is_none());
        let policy =
            relay_relevance_policy(Some(true), ["chan-a, chan-b", "thread-1"], Some("mentions"))
                .expect("policy");
        assert_eq!(policy.require_address, Some(true));
        assert_eq!(
            policy.free_response_scopes,
            vec![
                "chan-a".to_string(),
                "chan-b".to_string(),
                "thread-1".to_string()
            ]
        );
        assert!(policy.allow_other_bots);
        assert_eq!(
            serde_json::to_value(&policy).unwrap(),
            json!({
                "requireAddress": true,
                "freeResponseScopes": ["chan-a", "chan-b", "thread-1"],
                "allowOtherBots": true
            })
        );
    }
}
