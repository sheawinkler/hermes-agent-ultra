//! LobeHub skill source.

use bytes::Bytes;
use hermes_core::AgentError;

use super::registry::build_lobehub_skill_markdown;
use super::types::LobeHubAgentResponse;

pub(crate) async fn fetch_lobehub_skill_files(
    client: &reqwest::Client,
    slug: &str,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let url = format!("https://chat-agents.lobehub.com/{}.json", slug);
    let resp = client
        .get(&url)
        .header("Accept", "application/json,text/plain,*/*")
        .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("LobeHub request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "LobeHub lookup failed for '{}' ({}): {}",
            slug, status, body
        )));
    }
    let payload = resp
        .json::<LobeHubAgentResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid LobeHub payload: {}", e)))?;
    let md = build_lobehub_skill_markdown(&payload, slug);
    Ok(vec![("SKILL.md".to_string(), Bytes::from(md))])
}
