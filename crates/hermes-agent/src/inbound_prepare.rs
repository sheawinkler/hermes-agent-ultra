//! Inbound message orchestration (vision routing, auxiliary pre-analysis).

use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{
    transport_fallback_message, AgentError, InboundEvent, InboundMessagePreparer,
    InboundPrepareContext, Message,
};
use hermes_intelligence::auxiliary::{AuxiliaryClient, AuxiliaryRequest, AuxiliaryTask};
use hermes_intelligence::image_routing::{
    build_native_content_parts, decide_image_input_mode,
};
use hermes_intelligence::vision_media;
use tracing::debug;

const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";

const VISION_ANALYSIS_PROMPT: &str = "Describe everything visible in this image in thorough detail. \
Include any text, code, data, objects, people, layout, colors, \
and any other notable visual information.";

/// Agent-side inbound preparer: native multimodal or auxiliary vision enrich.
pub struct AgentInboundPreparer {
    auxiliary: Arc<AuxiliaryClient>,
}

impl AgentInboundPreparer {
    pub fn new(auxiliary: Arc<AuxiliaryClient>) -> Self {
        Self { auxiliary }
    }

    fn image_paths(event: &InboundEvent) -> Vec<String> {
        event
            .media_urls
            .iter()
            .zip(event.media_types.iter())
            .filter_map(|(url, ty)| {
                let ty = ty.trim().to_lowercase();
                if ty.starts_with("image/") || ty == "image" {
                    Some(url.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn message_from_native_parts(parts: &[serde_json::Value]) -> Message {
        let payload = serde_json::to_string(parts).unwrap_or_else(|_| "[]".to_string());
        Message::user(format!("{ACP_MULTIMODAL_PREFIX}{payload}"))
    }

    async fn enrich_with_vision(&self, user_text: &str, image_paths: &[String]) -> String {
        let mut enriched_parts = Vec::new();
        for path in image_paths {
            match self.analyze_one(path).await {
                Ok(description) => {
                    enriched_parts.push(format!(
                        "[The user sent an image~ Here's what I can see:\n{description}]\n\
                         [If you need a closer look, use vision_analyze with image_url: {path} ~]"
                    ));
                }
                Err(e) => {
                    tracing::warn!(path = %path, error = %e, "Vision auto-analysis failed");
                    enriched_parts.push(format!(
                        "[The user sent an image but I couldn't quite see it this time (>_<) \
                         You can try looking at it yourself with vision_analyze using image_url: {path}]"
                    ));
                }
            }
        }
        if enriched_parts.is_empty() {
            return user_text.to_string();
        }
        let prefix = enriched_parts.join("\n\n");
        if user_text.trim().is_empty() {
            prefix
        } else {
            format!("{prefix}\n\n{user_text}")
        }
    }

    async fn analyze_one(&self, path: &str) -> Result<String, AgentError> {
        let image_part = vision_media::encode_image_url_part(path)
            .await
            .map_err(AgentError::ToolExecution)?;
        let parts = serde_json::json!([
            {"type": "text", "text": VISION_ANALYSIS_PROMPT},
            image_part
        ]);
        let payload = serde_json::to_string(&parts)
            .map_err(|e| AgentError::Config(format!("vision parts encode: {e}")))?;
        let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{payload}"))];
        let response = self
            .auxiliary
            .call(AuxiliaryRequest::new(AuxiliaryTask::Vision, messages))
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        response
            .text()
            .map(|s| s.to_string())
            .ok_or_else(|| AgentError::LlmApi("Vision model returned empty content".into()))
    }
}

#[async_trait]
impl InboundMessagePreparer for AgentInboundPreparer {
    async fn prepare(
        &self,
        event: &InboundEvent,
        ctx: &InboundPrepareContext,
    ) -> Result<Message, AgentError> {
        let image_paths = Self::image_paths(event);
        if image_paths.is_empty() {
            return Ok(transport_fallback_message(event));
        }

        let provider = ctx.provider.as_deref().unwrap_or("");
        let model = ctx.model.as_deref().unwrap_or("");
        let mode = decide_image_input_mode(
            provider,
            model,
            &ctx.image_input_mode,
            ctx.aux_vision_provider.as_deref(),
            ctx.aux_vision_model.as_deref(),
            ctx.aux_vision_base_url.as_deref(),
        );

        debug!(
            provider = provider,
            model = model,
            mode = mode,
            image_count = image_paths.len(),
            "inbound image routing"
        );

        if mode == "native" {
            debug!(
                provider = provider,
                model = model,
                "skipping auxiliary vision pre-analysis (native image mode)"
            );
            let (parts, _skipped) =
                build_native_content_parts(&event.text, &image_paths);
            if parts.is_empty() {
                return Ok(transport_fallback_message(event));
            }
            return Ok(Self::message_from_native_parts(&parts));
        }

        let enriched = self.enrich_with_vision(&event.text, &image_paths).await;
        Ok(Message::user(enriched))
    }
}
