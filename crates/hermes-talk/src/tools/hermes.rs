use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use crate::config::AipcTalkConfig;
use crate::error::{DemoError, Result};

#[derive(Debug, Serialize)]
struct TalkRequest {
    request_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TalkResponse {
    #[serde(default)]
    request_id: String,
    text: String,
    #[serde(default)]
    status: String,
}

pub async fn call_hermes(text: &str, config: &AipcTalkConfig) -> Result<String> {
    if !config.url.starts_with("ws://") && !config.url.starts_with("wss://") {
        return Err(DemoError::Tool(format!(
            "invalid hermes url '{}': must start with ws:// or wss://",
            config.url
        )));
    }

    info!(url = %config.url, %text, "call_hermes: connecting");

    let (ws_stream, _response) =
        tokio::time::timeout(Duration::from_secs(10), connect_async(config.url.as_str()))
            .await
            .map_err(|_| DemoError::Tool("hermes connection timeout (>10s)".to_string()))?
            .map_err(|e| DemoError::Tool(format!("hermes WS connect failed: {e}")))?;

    info!(url = %config.url, "hermes WS: connected");

    let (mut write, mut read) = ws_stream.split();

    let request_id = uuid::Uuid::new_v4().to_string();
    let req = TalkRequest {
        request_id: request_id.clone(),
        text: text.to_string(),
        model: config.model.clone(),
        provider: config.provider.clone(),
    };
    let req_json = serde_json::to_string(&req)
        .map_err(|e| DemoError::Tool(format!("hermes serialize request: {e}")))?;

    write
        .send(WsMessage::Text(req_json.into()))
        .await
        .map_err(|e| DemoError::Tool(format!("hermes send failed: {e}")))?;

    debug!(%request_id, "call_hermes: request sent, waiting for reply");

    let read_fut = read.next();
    let response_msg = match config.timeout_secs {
        Some(secs) => tokio::time::timeout(Duration::from_secs(secs), read_fut)
            .await
            .map_err(|_| DemoError::Tool(format!("hermes response timeout (>{secs}s)")))?,
        None => read_fut.await,
    }
    .ok_or_else(|| {
        warn!("hermes WS: stream ended unexpectedly");
        DemoError::Tool("hermes WS stream ended".to_string())
    })?
    .map_err(|e| DemoError::Tool(format!("hermes WS read error: {e}")))?;

    let response_text = match response_msg {
        WsMessage::Text(t) => {
            info!(%request_id, "hermes WS: response received");
            t.to_string()
        }
        WsMessage::Close(frame) => {
            warn!(
                reason = ?frame.as_ref().map(|f| f.reason.to_string()),
                "hermes WS: connection closed by server"
            );
            return Err(DemoError::Tool(format!(
                "hermes closed connection: {:?}",
                frame.map(|f| f.reason.to_string())
            )));
        }
        other => {
            warn!(msg_type = ?other, "hermes WS: unexpected message");
            return Err(DemoError::Tool(format!(
                "hermes unexpected message type: {other:?}"
            )));
        }
    };

    let response: TalkResponse = serde_json::from_str(&response_text).map_err(|e| {
        DemoError::Tool(format!(
            "hermes invalid JSON response: {e}, raw: {response_text}"
        ))
    })?;

    if response.request_id != request_id {
        warn!(
            expected = %request_id,
            got = %response.request_id,
            "call_hermes: request_id mismatch"
        );
    }

    if response.status != "ok" {
        warn!(status = %response.status, text = %response.text, "call_hermes: non-ok status");
    }

    if response.text.contains("did not respond in time")
        || response.text.contains("timeout")
        || response.text.contains("timed out")
    {
        return Err(DemoError::Tool(format!(
            "hermes agent timeout, will retry: {}",
            response.text
        )));
    }

    info!(len = response.text.len(), "call_hermes: got reply");
    Ok(response.text)
}
