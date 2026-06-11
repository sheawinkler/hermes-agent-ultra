//! Install fallback routing.

use hermes_core::AgentError;

use super::skills_sh::resolve_skills_sh_source;
use super::taps::resolve_skill_via_taps;
use super::types::{InstallFallbackSource, ResolvedSkillSource};

// ---------------------------------------------------------------------------
// Fallback router
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_install_via_fallback_router(
    client: &reqwest::Client,
    skill_name: &str,
    taps: &[String],
) -> Result<(ResolvedSkillSource, InstallFallbackSource), AgentError> {
    if let Ok(resolved) = resolve_skills_sh_source(client, skill_name).await {
        return Ok((resolved, InstallFallbackSource::SkillsSh));
    }
    let resolved = resolve_skill_via_taps(client, taps, skill_name).await?;
    Ok((resolved, InstallFallbackSource::Tap))
}
