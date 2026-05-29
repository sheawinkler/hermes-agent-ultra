//! Spotify Web API tools.
//!
//! These handlers port the upstream bundled Spotify plugin into the Rust
//! runtime. The Python plugin remains reference material only.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Map, Number, Value};
use std::sync::Arc;

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyHttpMethod {
    Get,
    Post,
    Put,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyApiRequest {
    pub method: SpotifyHttpMethod,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
    pub empty_response: Option<Value>,
}

impl SpotifyApiRequest {
    pub fn new(method: SpotifyHttpMethod, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            body: None,
            empty_response: None,
        }
    }

    fn with_query(mut self, key: &str, value: Option<String>) -> Self {
        if let Some(value) = value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            self.query.push((key.to_string(), value));
        }
        self
    }

    fn with_body(mut self, body: Value) -> Self {
        self.body = Some(strip_null_object(body));
        self
    }

    fn with_empty_response(mut self, value: Value) -> Self {
        self.empty_response = Some(value);
        self
    }
}

#[async_trait]
pub trait SpotifyBackend: Send + Sync {
    async fn call(&self, request: SpotifyApiRequest) -> Result<Value, ToolError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyTool {
    Playback,
    Devices,
    Queue,
    Search,
    Playlists,
    Albums,
    Library,
}

pub struct SpotifyHandler {
    tool: SpotifyTool,
    backend: Arc<dyn SpotifyBackend>,
}

impl SpotifyHandler {
    pub fn new(tool: SpotifyTool, backend: Arc<dyn SpotifyBackend>) -> Self {
        Self { tool, backend }
    }
}

#[async_trait]
impl ToolHandler for SpotifyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let result = match self.tool {
            SpotifyTool::Playback => handle_playback(&*self.backend, params).await?,
            SpotifyTool::Devices => handle_devices(&*self.backend, params).await?,
            SpotifyTool::Queue => handle_queue(&*self.backend, params).await?,
            SpotifyTool::Search => handle_search(&*self.backend, params).await?,
            SpotifyTool::Playlists => handle_playlists(&*self.backend, params).await?,
            SpotifyTool::Albums => handle_albums(&*self.backend, params).await?,
            SpotifyTool::Library => handle_library(&*self.backend, params).await?,
        };
        Ok(result.to_string())
    }

    fn schema(&self) -> ToolSchema {
        self.tool.schema()
    }
}

impl SpotifyTool {
    fn schema(self) -> ToolSchema {
        let common_string = json!({"type": "string"});
        match self {
            Self::Playback => {
                let mut props = IndexMap::new();
                props.insert(
                    "action".into(),
                    json!({
                        "type": "string",
                        "enum": ["get_state", "get_currently_playing", "play", "pause", "next", "previous", "seek", "set_repeat", "set_shuffle", "set_volume", "recently_played"]
                    }),
                );
                props.insert("device_id".into(), common_string.clone());
                props.insert("market".into(), common_string.clone());
                props.insert("context_uri".into(), common_string.clone());
                props.insert(
                    "uris".into(),
                    json!({"type": "array", "items": common_string.clone()}),
                );
                props.insert("offset".into(), json!({"type": "object"}));
                props.insert("position_ms".into(), json!({"type": "integer"}));
                props.insert(
                    "state".into(),
                    json!({
                        "description": "For set_repeat use track/context/off. For set_shuffle use boolean-like true/false.",
                        "oneOf": [{"type": "string"}, {"type": "boolean"}]
                    }),
                );
                props.insert("volume_percent".into(), json!({"type": "integer"}));
                props.insert(
                    "limit".into(),
                    json!({"type": "integer", "description": "For recently_played: number of tracks (max 50)"}),
                );
                props.insert(
                    "after".into(),
                    json!({"type": "integer", "description": "For recently_played: Unix ms cursor (after this timestamp)"}),
                );
                props.insert(
                    "before".into(),
                    json!({"type": "integer", "description": "For recently_played: Unix ms cursor (before this timestamp)"}),
                );
                tool_schema(
                    "spotify_playback",
                    "Control Spotify playback, inspect the active playback state, or fetch recently played tracks.",
                    JsonSchema::object(props, vec!["action".into()]),
                )
            }
            Self::Devices => {
                let mut props = IndexMap::new();
                props.insert(
                    "action".into(),
                    json!({"type": "string", "enum": ["list", "transfer"]}),
                );
                props.insert("device_id".into(), common_string.clone());
                props.insert("play".into(), json!({"type": "boolean"}));
                tool_schema(
                    "spotify_devices",
                    "List Spotify Connect devices or transfer playback to a different device.",
                    JsonSchema::object(props, vec!["action".into()]),
                )
            }
            Self::Queue => {
                let mut props = IndexMap::new();
                props.insert(
                    "action".into(),
                    json!({"type": "string", "enum": ["get", "add"]}),
                );
                props.insert("uri".into(), common_string.clone());
                props.insert("device_id".into(), common_string.clone());
                tool_schema(
                    "spotify_queue",
                    "Inspect the user's Spotify queue or add an item to it.",
                    JsonSchema::object(props, vec!["action".into()]),
                )
            }
            Self::Search => {
                let mut props = IndexMap::new();
                props.insert("query".into(), common_string.clone());
                props.insert(
                    "types".into(),
                    json!({"type": "array", "items": common_string.clone()}),
                );
                props.insert("type".into(), common_string.clone());
                props.insert("limit".into(), json!({"type": "integer"}));
                props.insert("offset".into(), json!({"type": "integer"}));
                props.insert("market".into(), common_string.clone());
                props.insert("include_external".into(), common_string.clone());
                tool_schema(
                    "spotify_search",
                    "Search the Spotify catalog for tracks, albums, artists, playlists, shows, or episodes.",
                    JsonSchema::object(props, vec!["query".into()]),
                )
            }
            Self::Playlists => {
                let mut props = IndexMap::new();
                props.insert(
                    "action".into(),
                    json!({"type": "string", "enum": ["list", "get", "create", "add_items", "remove_items", "update_details"]}),
                );
                props.insert("playlist_id".into(), common_string.clone());
                props.insert("market".into(), common_string.clone());
                props.insert("limit".into(), json!({"type": "integer"}));
                props.insert("offset".into(), json!({"type": "integer"}));
                props.insert("name".into(), common_string.clone());
                props.insert("description".into(), common_string.clone());
                props.insert("public".into(), json!({"type": "boolean"}));
                props.insert("collaborative".into(), json!({"type": "boolean"}));
                props.insert(
                    "uris".into(),
                    json!({"type": "array", "items": common_string.clone()}),
                );
                props.insert("position".into(), json!({"type": "integer"}));
                props.insert("snapshot_id".into(), common_string.clone());
                tool_schema(
                    "spotify_playlists",
                    "List, inspect, create, update, and modify Spotify playlists.",
                    JsonSchema::object(props, vec!["action".into()]),
                )
            }
            Self::Albums => {
                let mut props = IndexMap::new();
                props.insert(
                    "action".into(),
                    json!({"type": "string", "enum": ["get", "tracks"]}),
                );
                props.insert("album_id".into(), common_string.clone());
                props.insert("id".into(), common_string.clone());
                props.insert("market".into(), common_string.clone());
                props.insert("limit".into(), json!({"type": "integer"}));
                props.insert("offset".into(), json!({"type": "integer"}));
                tool_schema(
                    "spotify_albums",
                    "Fetch Spotify album metadata or album tracks.",
                    JsonSchema::object(props, vec!["action".into()]),
                )
            }
            Self::Library => {
                let mut props = IndexMap::new();
                props.insert(
                    "kind".into(),
                    json!({"type": "string", "enum": ["tracks", "albums"], "description": "Which library to operate on"}),
                );
                props.insert(
                    "action".into(),
                    json!({"type": "string", "enum": ["list", "save", "remove"]}),
                );
                props.insert("limit".into(), json!({"type": "integer"}));
                props.insert("offset".into(), json!({"type": "integer"}));
                props.insert("market".into(), common_string.clone());
                props.insert(
                    "uris".into(),
                    json!({"type": "array", "items": common_string.clone()}),
                );
                props.insert(
                    "ids".into(),
                    json!({"type": "array", "items": common_string.clone()}),
                );
                props.insert(
                    "items".into(),
                    json!({"type": "array", "items": common_string}),
                );
                tool_schema(
                    "spotify_library",
                    "List, save, or remove the user's saved Spotify tracks or albums. Use `kind` to select which.",
                    JsonSchema::object(props, vec!["kind".into(), "action".into()]),
                )
            }
        }
    }
}

async fn handle_playback(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let action = action(&params, "get_state");
    match action.as_str() {
        "get_state" => {
            let payload = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/me/player")
                        .with_query("market", optional_string(&params, "market"))
                        .with_empty_response(json!({
                            "status_code": 204,
                            "empty": true,
                            "message": "No active Spotify playback session was found. Open Spotify on a device and start playback, or transfer playback to an available device."
                        })),
                )
                .await?;
            Ok(describe_empty_playback(payload, &action))
        }
        "get_currently_playing" => {
            let payload = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/me/player/currently-playing")
                        .with_query("market", optional_string(&params, "market"))
                        .with_empty_response(json!({
                            "status_code": 204,
                            "empty": true,
                            "message": "Spotify is not currently playing anything. Start playback in Spotify and try again."
                        })),
                )
                .await?;
            Ok(describe_empty_playback(payload, &action))
        }
        "play" => {
            let mut body = Map::new();
            if let Some(context_uri) = optional_string(&params, "context_uri") {
                let context_type = infer_context_type(&context_uri);
                body.insert(
                    "context_uri".into(),
                    Value::String(normalize_spotify_uri(&context_uri, context_type)?),
                );
            }
            if params.get("uris").is_some() {
                let uris = normalize_spotify_uris(as_list(params.get("uris")), Some("track"))?;
                body.insert(
                    "uris".into(),
                    Value::Array(uris.into_iter().map(Value::String).collect()),
                );
            }
            if let Some(offset) = params.get("offset").and_then(Value::as_object) {
                let clean: Map<String, Value> = offset
                    .iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                if !clean.is_empty() {
                    body.insert("offset".into(), Value::Object(clean));
                }
            }
            if params.get("position_ms").is_some() {
                body.insert(
                    "position_ms".into(),
                    Value::Number(Number::from(required_i64(&params, "position_ms")?)),
                );
            }
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player/play")
                        .with_query("device_id", optional_string(&params, "device_id"))
                        .with_body(Value::Object(body)),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        "pause" => {
            playback_command(
                backend,
                &action,
                SpotifyHttpMethod::Put,
                "/me/player/pause",
                &params,
            )
            .await
        }
        "next" => {
            playback_command(
                backend,
                &action,
                SpotifyHttpMethod::Post,
                "/me/player/next",
                &params,
            )
            .await
        }
        "previous" => {
            playback_command(
                backend,
                &action,
                SpotifyHttpMethod::Post,
                "/me/player/previous",
                &params,
            )
            .await
        }
        "seek" => {
            let position_ms = required_i64(&params, "position_ms")?;
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player/seek")
                        .with_query("position_ms", Some(position_ms.to_string()))
                        .with_query("device_id", optional_string(&params, "device_id")),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        "set_repeat" => {
            let state = optional_string(&params, "state")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !matches!(state.as_str(), "track" | "context" | "off") {
                return Err(ToolError::InvalidParams(
                    "state must be one of: track, context, off".into(),
                ));
            }
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player/repeat")
                        .with_query("state", Some(state))
                        .with_query("device_id", optional_string(&params, "device_id")),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        "set_shuffle" => {
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player/shuffle")
                        .with_query(
                            "state",
                            Some(coerce_bool(params.get("state"), false).to_string()),
                        )
                        .with_query("device_id", optional_string(&params, "device_id")),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        "set_volume" => {
            let volume = required_i64(&params, "volume_percent")?.clamp(0, 100);
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player/volume")
                        .with_query("volume_percent", Some(volume.to_string()))
                        .with_query("device_id", optional_string(&params, "device_id")),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        "recently_played" => {
            let after = optional_i64(&params, "after")?;
            let before = optional_i64(&params, "before")?;
            if after.is_some() && before.is_some() {
                return Err(ToolError::InvalidParams(
                    "Provide only one of 'after' or 'before'".into(),
                ));
            }
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/me/player/recently-played")
                        .with_query(
                            "limit",
                            Some(coerce_limit(params.get("limit"), 20, 1, 50).to_string()),
                        )
                        .with_query("after", after.map(|v| v.to_string()))
                        .with_query("before", before.map(|v| v.to_string())),
                )
                .await
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_playback action: {action}"
        ))),
    }
}

async fn playback_command(
    backend: &dyn SpotifyBackend,
    action: &str,
    method: SpotifyHttpMethod,
    path: &str,
    params: &Value,
) -> Result<Value, ToolError> {
    let result = backend
        .call(
            SpotifyApiRequest::new(method, path)
                .with_query("device_id", optional_string(params, "device_id")),
        )
        .await?;
    Ok(action_result(action, result))
}

async fn handle_devices(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let action = action(&params, "list");
    match action.as_str() {
        "list" => {
            backend
                .call(SpotifyApiRequest::new(
                    SpotifyHttpMethod::Get,
                    "/me/player/devices",
                ))
                .await
        }
        "transfer" => {
            let device_id = required_string(
                &params,
                "device_id",
                "device_id is required for action='transfer'",
            )?;
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/player").with_body(json!({
                        "device_ids": [device_id],
                        "play": coerce_bool(params.get("play"), false)
                    })),
                )
                .await?;
            Ok(action_result(&action, result))
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_devices action: {action}"
        ))),
    }
}

async fn handle_queue(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let action = action(&params, "get");
    match action.as_str() {
        "get" => {
            backend
                .call(SpotifyApiRequest::new(
                    SpotifyHttpMethod::Get,
                    "/me/player/queue",
                ))
                .await
        }
        "add" => {
            let uri = normalize_spotify_uri(
                &required_string(&params, "uri", "Spotify URI/url/id is required.")?,
                None,
            )?;
            let result = backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Post, "/me/player/queue")
                        .with_query("uri", Some(uri.clone()))
                        .with_query("device_id", optional_string(&params, "device_id")),
                )
                .await?;
            Ok(json!({"success": true, "action": action, "uri": uri, "result": result}))
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_queue action: {action}"
        ))),
    }
}

async fn handle_search(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let query = required_string(&params, "query", "query is required")?;
    let valid = [
        "album",
        "artist",
        "playlist",
        "track",
        "show",
        "episode",
        "audiobook",
    ];
    let raw_types = params
        .get("types")
        .or_else(|| params.get("type"))
        .map(|value| as_list(Some(value)))
        .unwrap_or_else(|| vec!["track".to_string()]);
    let search_types: Vec<String> = raw_types
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| valid.contains(&value.as_str()))
        .collect();
    if search_types.is_empty() {
        return Err(ToolError::InvalidParams(
            "types must contain one or more of: album, artist, playlist, track, show, episode, audiobook".into(),
        ));
    }

    backend
        .call(
            SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/search")
                .with_query("q", Some(query))
                .with_query("type", Some(search_types.join(",")))
                .with_query(
                    "limit",
                    Some(coerce_limit(params.get("limit"), 10, 1, 50).to_string()),
                )
                .with_query(
                    "offset",
                    Some(nonnegative_i64(params.get("offset"), 0)?.to_string()),
                )
                .with_query("market", optional_string(&params, "market"))
                .with_query(
                    "include_external",
                    optional_string(&params, "include_external"),
                ),
        )
        .await
}

async fn handle_playlists(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let action = action(&params, "list");
    match action.as_str() {
        "list" => {
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/me/playlists")
                        .with_query(
                            "limit",
                            Some(coerce_limit(params.get("limit"), 20, 1, 50).to_string()),
                        )
                        .with_query(
                            "offset",
                            Some(nonnegative_i64(params.get("offset"), 0)?.to_string()),
                        ),
                )
                .await
        }
        "get" => {
            let playlist_id = normalize_spotify_id(
                &required_string(&params, "playlist_id", "Spotify id/uri/url is required.")?,
                Some("playlist"),
            )?;
            backend
                .call(
                    SpotifyApiRequest::new(
                        SpotifyHttpMethod::Get,
                        format!("/playlists/{playlist_id}"),
                    )
                    .with_query("market", optional_string(&params, "market")),
                )
                .await
        }
        "create" => {
            let name = required_string(&params, "name", "name is required for action='create'")?;
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Post, "/me/playlists").with_body(
                        json!({
                            "name": name,
                            "public": coerce_bool(params.get("public"), false),
                            "collaborative": coerce_bool(params.get("collaborative"), false),
                            "description": optional_string(&params, "description")
                        }),
                    ),
                )
                .await
        }
        "add_items" => {
            let playlist_id = normalize_spotify_id(
                &required_string(&params, "playlist_id", "Spotify id/uri/url is required.")?,
                Some("playlist"),
            )?;
            let uris = normalize_spotify_uris(as_list(params.get("uris")), None)?;
            let mut body = Map::new();
            body.insert(
                "uris".into(),
                Value::Array(uris.into_iter().map(Value::String).collect()),
            );
            if params.get("position").is_some() {
                body.insert(
                    "position".into(),
                    Value::Number(Number::from(required_i64(&params, "position")?)),
                );
            }
            backend
                .call(
                    SpotifyApiRequest::new(
                        SpotifyHttpMethod::Post,
                        format!("/playlists/{playlist_id}/items"),
                    )
                    .with_body(Value::Object(body)),
                )
                .await
        }
        "remove_items" => {
            let playlist_id = normalize_spotify_id(
                &required_string(&params, "playlist_id", "Spotify id/uri/url is required.")?,
                Some("playlist"),
            )?;
            let uris = normalize_spotify_uris(as_list(params.get("uris")), None)?;
            backend
                .call(
                    SpotifyApiRequest::new(
                        SpotifyHttpMethod::Delete,
                        format!("/playlists/{playlist_id}/items"),
                    )
                    .with_body(json!({
                        "items": uris.into_iter().map(|uri| json!({"uri": uri})).collect::<Vec<_>>(),
                        "snapshot_id": optional_string(&params, "snapshot_id")
                    })),
                )
                .await
        }
        "update_details" => {
            let playlist_id = normalize_spotify_id(
                &required_string(&params, "playlist_id", "Spotify id/uri/url is required.")?,
                Some("playlist"),
            )?;
            let mut body = Map::new();
            insert_raw_non_null(&mut body, &params, "name");
            insert_raw_non_null(&mut body, &params, "public");
            insert_raw_non_null(&mut body, &params, "collaborative");
            insert_raw_non_null(&mut body, &params, "description");
            backend
                .call(
                    SpotifyApiRequest::new(
                        SpotifyHttpMethod::Put,
                        format!("/playlists/{playlist_id}"),
                    )
                    .with_body(Value::Object(body)),
                )
                .await
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_playlists action: {action}"
        ))),
    }
}

async fn handle_albums(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let action = action(&params, "get");
    let album_id = normalize_spotify_id(
        &params
            .get("album_id")
            .or_else(|| params.get("id"))
            .and_then(value_to_string)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::InvalidParams("Spotify id/uri/url is required.".into()))?,
        Some("album"),
    )?;
    match action.as_str() {
        "get" => {
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, format!("/albums/{album_id}"))
                        .with_query("market", optional_string(&params, "market")),
                )
                .await
        }
        "tracks" => {
            backend
                .call(
                    SpotifyApiRequest::new(
                        SpotifyHttpMethod::Get,
                        format!("/albums/{album_id}/tracks"),
                    )
                    .with_query(
                        "limit",
                        Some(coerce_limit(params.get("limit"), 20, 1, 50).to_string()),
                    )
                    .with_query(
                        "offset",
                        Some(nonnegative_i64(params.get("offset"), 0)?.to_string()),
                    )
                    .with_query("market", optional_string(&params, "market")),
                )
                .await
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_albums action: {action}"
        ))),
    }
}

async fn handle_library(backend: &dyn SpotifyBackend, params: Value) -> Result<Value, ToolError> {
    let kind = optional_string(&params, "kind")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(kind.as_str(), "tracks" | "albums") {
        return Err(ToolError::InvalidParams(
            "kind must be one of: tracks, albums".into(),
        ));
    }
    let action = action(&params, "list");
    let item_type = if kind == "tracks" { "track" } else { "album" };
    match action.as_str() {
        "list" => {
            let path = if kind == "tracks" {
                "/me/tracks"
            } else {
                "/me/albums"
            };
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Get, path)
                        .with_query(
                            "limit",
                            Some(coerce_limit(params.get("limit"), 20, 1, 50).to_string()),
                        )
                        .with_query(
                            "offset",
                            Some(nonnegative_i64(params.get("offset"), 0)?.to_string()),
                        )
                        .with_query("market", optional_string(&params, "market")),
                )
                .await
        }
        "save" => {
            let uris = normalize_spotify_uris(
                as_list(params.get("uris").or_else(|| params.get("items"))),
                Some(item_type),
            )?;
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Put, "/me/library")
                        .with_query("uris", Some(uris.join(","))),
                )
                .await
        }
        "remove" => {
            let ids = as_list(params.get("ids").or_else(|| params.get("items")));
            if ids.is_empty() {
                return Err(ToolError::InvalidParams(
                    "ids/items is required for action='remove'".into(),
                ));
            }
            let ids: Vec<String> = ids
                .into_iter()
                .map(|item| normalize_spotify_id(&item, Some(item_type)))
                .collect::<Result<_, _>>()?;
            let uris: Vec<String> = ids
                .into_iter()
                .map(|id| format!("spotify:{item_type}:{id}"))
                .collect();
            backend
                .call(
                    SpotifyApiRequest::new(SpotifyHttpMethod::Delete, "/me/library")
                        .with_query("uris", Some(uris.join(","))),
                )
                .await
        }
        _ => Err(ToolError::InvalidParams(format!(
            "Unknown spotify_library action: {action}"
        ))),
    }
}

fn action(params: &Value, default: &str) -> String {
    optional_string(params, "action")
        .unwrap_or_else(|| default.to_string())
        .to_ascii_lowercase()
}

fn action_result(action: &str, result: Value) -> Value {
    json!({"success": true, "action": action, "result": result})
}

fn describe_empty_playback(payload: Value, action: &str) -> Value {
    let Some(obj) = payload.as_object() else {
        return payload;
    };
    if obj.get("empty").and_then(Value::as_bool) != Some(true) {
        return payload;
    }
    let status_code = obj
        .get("status_code")
        .cloned()
        .unwrap_or_else(|| json!(204));
    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    match action {
        "get_currently_playing" => json!({
            "success": true,
            "action": action,
            "is_playing": false,
            "status_code": status_code,
            "message": message.unwrap_or_else(|| "Spotify is not currently playing anything.".to_string())
        }),
        "get_state" => json!({
            "success": true,
            "action": action,
            "has_active_device": false,
            "status_code": status_code,
            "message": message.unwrap_or_else(|| "No active Spotify playback session was found.".to_string())
        }),
        _ => payload,
    }
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(value_to_string)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn required_string(params: &Value, key: &str, message: &str) -> Result<String, ToolError> {
    optional_string(params, key).ok_or_else(|| ToolError::InvalidParams(message.into()))
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn optional_i64(params: &Value, key: &str) -> Result<Option<i64>, ToolError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value_as_i64(value)
            .map(Some)
            .ok_or_else(|| ToolError::InvalidParams(format!("{key} must be an integer"))),
    }
}

fn required_i64(params: &Value, key: &str) -> Result<i64, ToolError> {
    optional_i64(params, key)?.ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "{key} is required for action='{}'",
            action(params, "")
        ))
    })
}

fn nonnegative_i64(raw: Option<&Value>, default: i64) -> Result<i64, ToolError> {
    match raw {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value_as_i64(value)
            .map(|n| n.max(0))
            .ok_or_else(|| ToolError::InvalidParams("offset must be an integer".into())),
    }
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn coerce_limit(raw: Option<&Value>, default: i64, minimum: i64, maximum: i64) -> i64 {
    raw.and_then(value_as_i64)
        .unwrap_or(default)
        .clamp(minimum, maximum)
}

fn coerce_bool(raw: Option<&Value>, default: bool) -> bool {
    match raw {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

fn as_list(raw: Option<&Value>) -> Vec<String> {
    match raw {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(value_to_string)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(value) => value_to_string(value)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
    }
}

fn infer_context_type(value: &str) -> Option<&'static str> {
    if value.starts_with("spotify:album:") || value.contains("/album/") {
        Some("album")
    } else if value.starts_with("spotify:playlist:") || value.contains("/playlist/") {
        Some("playlist")
    } else if value.starts_with("spotify:artist:") || value.contains("/artist/") {
        Some("artist")
    } else {
        None
    }
}

fn normalize_spotify_id(value: &str, expected_type: Option<&str>) -> Result<String, ToolError> {
    let cleaned = value.trim();
    if cleaned.is_empty() {
        return Err(ToolError::InvalidParams(
            "Spotify id/uri/url is required.".into(),
        ));
    }
    if let Some(rest) = cleaned.strip_prefix("spotify:") {
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() >= 2 {
            let item_type = parts[0];
            if let Some(expected) = expected_type {
                if item_type != expected {
                    return Err(ToolError::InvalidParams(format!(
                        "Expected a Spotify {expected}, got {item_type}."
                    )));
                }
            }
            return Ok(parts[1].to_string());
        }
    }
    if cleaned.contains("open.spotify.com") {
        if let Ok(parsed) = reqwest::Url::parse(cleaned) {
            let parts: Vec<&str> = parsed
                .path_segments()
                .map(|segments| segments.filter(|part| !part.is_empty()).collect())
                .unwrap_or_default();
            if parts.len() >= 2 {
                let item_type = parts[0];
                let item_id = parts[1];
                if let Some(expected) = expected_type {
                    if item_type != expected {
                        return Err(ToolError::InvalidParams(format!(
                            "Expected a Spotify {expected}, got {item_type}."
                        )));
                    }
                }
                return Ok(item_id.to_string());
            }
        }
    }
    Ok(cleaned.to_string())
}

fn normalize_spotify_uri(value: &str, expected_type: Option<&str>) -> Result<String, ToolError> {
    let cleaned = value.trim();
    if cleaned.is_empty() {
        return Err(ToolError::InvalidParams(
            "Spotify URI/url/id is required.".into(),
        ));
    }
    if let Some(rest) = cleaned.strip_prefix("spotify:") {
        if let Some(expected) = expected_type {
            let parts: Vec<&str> = rest.split(':').collect();
            if parts.len() >= 2 && parts[0] != expected {
                return Err(ToolError::InvalidParams(format!(
                    "Expected a Spotify {expected}, got {}.",
                    parts[0]
                )));
            }
        }
        return Ok(cleaned.to_string());
    }
    let item_id = normalize_spotify_id(cleaned, expected_type)?;
    Ok(expected_type
        .map(|item_type| format!("spotify:{item_type}:{item_id}"))
        .unwrap_or(item_id))
}

fn normalize_spotify_uris(
    values: Vec<String>,
    expected_type: Option<&str>,
) -> Result<Vec<String>, ToolError> {
    let mut uris = Vec::new();
    for value in values {
        let uri = normalize_spotify_uri(&value, expected_type)?;
        if !uris.contains(&uri) {
            uris.push(uri);
        }
    }
    if uris.is_empty() {
        return Err(ToolError::InvalidParams(
            "At least one Spotify item is required.".into(),
        ));
    }
    Ok(uris)
}

fn insert_raw_non_null(out: &mut Map<String, Value>, params: &Value, key: &str) {
    if let Some(value) = params.get(key).filter(|value| !value.is_null()) {
        out.insert(key.to_string(), value.clone());
    }
}

fn strip_null_object(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSpotifyBackend {
        calls: Mutex<Vec<SpotifyApiRequest>>,
        responses: Mutex<Vec<Value>>,
    }

    impl RecordingSpotifyBackend {
        fn with_response(value: Value) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(vec![value]),
            }
        }

        fn take_call(&self) -> SpotifyApiRequest {
            self.calls.lock().unwrap().remove(0)
        }
    }

    #[async_trait]
    impl SpotifyBackend for RecordingSpotifyBackend {
        async fn call(&self, request: SpotifyApiRequest) -> Result<Value, ToolError> {
            self.calls.lock().unwrap().push(request);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn playback_play_shapes_context_and_track_uris() {
        let backend = Arc::new(RecordingSpotifyBackend::default());
        let handler = SpotifyHandler::new(SpotifyTool::Playback, backend.clone());

        let out = handler
            .execute(json!({
                "action": "play",
                "device_id": "speaker-1",
                "context_uri": "https://open.spotify.com/album/album123?si=x",
                "uris": ["track-a", "spotify:track:track-a", "spotify:track:track-b"],
                "position_ms": "2500"
            }))
            .await
            .unwrap();

        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["success"], true);
        let call = backend.take_call();
        assert_eq!(call.method, SpotifyHttpMethod::Put);
        assert_eq!(call.path, "/me/player/play");
        assert_eq!(call.query, vec![("device_id".into(), "speaker-1".into())]);
        let body = call.body.unwrap();
        assert_eq!(body["context_uri"], "spotify:album:album123");
        assert_eq!(
            body["uris"],
            json!(["spotify:track:track-a", "spotify:track:track-b"])
        );
        assert_eq!(body["position_ms"], 2500);
    }

    #[tokio::test]
    async fn search_filters_types_and_clamps_numbers() {
        let backend = Arc::new(RecordingSpotifyBackend::default());
        let handler = SpotifyHandler::new(SpotifyTool::Search, backend.clone());

        handler
            .execute(json!({
                "query": "glass beams",
                "types": ["track", "garbage", "album"],
                "limit": 99,
                "offset": -10,
                "market": "US"
            }))
            .await
            .unwrap();

        let call = backend.take_call();
        assert_eq!(call.path, "/search");
        assert_eq!(
            call.query,
            vec![
                ("q".into(), "glass beams".into()),
                ("type".into(), "track,album".into()),
                ("limit".into(), "50".into()),
                ("offset".into(), "0".into()),
                ("market".into(), "US".into())
            ]
        );
    }

    #[tokio::test]
    async fn empty_playback_response_is_described() {
        let backend = Arc::new(RecordingSpotifyBackend::with_response(json!({
            "status_code": 204,
            "empty": true,
            "message": "nothing active"
        })));
        let handler = SpotifyHandler::new(SpotifyTool::Playback, backend);

        let out = handler
            .execute(json!({"action": "get_state"}))
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["success"], true);
        assert_eq!(out["has_active_device"], false);
        assert_eq!(out["message"], "nothing active");
    }

    #[tokio::test]
    async fn library_remove_normalizes_album_ids() {
        let backend = Arc::new(RecordingSpotifyBackend::default());
        let handler = SpotifyHandler::new(SpotifyTool::Library, backend.clone());

        handler
            .execute(json!({
                "kind": "albums",
                "action": "remove",
                "items": ["https://open.spotify.com/album/abc", "spotify:album:def"]
            }))
            .await
            .unwrap();

        let call = backend.take_call();
        assert_eq!(call.method, SpotifyHttpMethod::Delete);
        assert_eq!(call.path, "/me/library");
        assert_eq!(
            call.query,
            vec![("uris".into(), "spotify:album:abc,spotify:album:def".into())]
        );
    }

    #[tokio::test]
    async fn validation_errors_match_upstream_contracts() {
        let backend = Arc::new(RecordingSpotifyBackend::default());
        let playback = SpotifyHandler::new(SpotifyTool::Playback, backend.clone());
        let err = playback
            .execute(json!({"action": "set_repeat", "state": "all"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("state must be one of"));

        let library = SpotifyHandler::new(SpotifyTool::Library, backend);
        let err = library
            .execute(json!({"kind": "tracks", "action": "remove"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ids/items is required"));
    }

    #[test]
    fn schemas_keep_upstream_tool_names() {
        let backend = Arc::new(RecordingSpotifyBackend::default());
        let names: Vec<String> = [
            SpotifyTool::Playback,
            SpotifyTool::Devices,
            SpotifyTool::Queue,
            SpotifyTool::Search,
            SpotifyTool::Playlists,
            SpotifyTool::Albums,
            SpotifyTool::Library,
        ]
        .into_iter()
        .map(|tool| SpotifyHandler::new(tool, backend.clone()).schema().name)
        .collect();

        assert_eq!(
            names,
            vec![
                "spotify_playback",
                "spotify_devices",
                "spotify_queue",
                "spotify_search",
                "spotify_playlists",
                "spotify_albums",
                "spotify_library"
            ]
        );
    }
}
