//! Skill identifier and path parsing.

use hermes_core::AgentError;

use super::types::SkillTapSpec;

pub(crate) fn parse_skill_tap_spec(raw: &str) -> Option<SkillTapSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (base, override_path) = if let Some((lhs, rhs)) = trimmed.split_once("::") {
        (lhs.trim(), Some(rhs.trim()))
    } else {
        (trimmed, None)
    };

    let (repo, mut path) = if let Some(rest) = base
        .strip_prefix("https://github.com/")
        .or_else(|| base.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() >= 5 && segments[2] == "tree" {
            segments[4..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    } else {
        let segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() > 2 {
            segments[2..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    };

    if let Some(override_path) = override_path {
        path = override_path.to_string();
    }

    Some(SkillTapSpec {
        repo,
        path: path.trim_matches('/').to_string(),
    })
}

pub(crate) fn parse_skill_name_and_version(spec: &str) -> (String, Option<String>) {
    let trimmed = spec.trim();
    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty() && !version.is_empty() && !name.starts_with("https://") {
            return (name.to_string(), Some(version.to_string()));
        }
    }
    (trimmed.to_string(), None)
}

pub(crate) fn looks_like_github_repo_slug(token: &str) -> bool {
    let parts: Vec<&str> = token.split('/').filter(|s| !s.is_empty()).collect();
    parts.len() == 2
}

pub(crate) fn parse_explicit_github_skill(spec: &str) -> Option<(String, Option<String>, String)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Registry-prefixed identifiers (official/..., skills.sh/..., etc.)
    // must not be treated as direct GitHub owner/repo/path slugs.
    if parse_registry_prefixed_skill(trimmed).is_some() {
        return None;
    }

    if let Some(rest) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let repo = format!("{}/{}", segments[0], segments[1]);
        if segments.len() >= 5 && segments[2] == "tree" {
            let branch = segments[3].to_string();
            let path = segments[4..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, Some(branch), path));
        }
        if segments.len() > 2 {
            let path = segments[2..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, None, path));
        }
        return None;
    }

    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 3 {
        let repo = format!("{}/{}", segments[0], segments[1]);
        let path = segments[2..].join("/");
        if path.is_empty() {
            return None;
        }
        return Some((repo, None, path));
    }

    None
}

pub(crate) fn sanitize_skill_install_name(source: &str) -> String {
    let raw = source
        .trim()
        .split('/')
        .next_back()
        .unwrap_or(source)
        .trim();
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if out.ends_with('_') {
            continue;
        } else {
            out.push('_');
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        "skill".to_string()
    } else {
        normalized
    }
}

pub(crate) fn ensure_safe_relative_path(path: &str) -> Result<(), AgentError> {
    if path.is_empty() {
        return Err(AgentError::Config("Empty path in skill bundle.".into()));
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(AgentError::Config(format!(
            "Unsafe path in skill bundle: {}",
            path
        )));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(AgentError::Config(format!(
                "Unsafe path segment in skill bundle: {}",
                path
            )));
        }
    }
    Ok(())
}

pub(crate) fn parse_registry_prefixed_skill(spec: &str) -> Option<(String, String)> {
    let (prefix, rest) = spec.split_once('/')?;
    let normalized = prefix.trim().to_ascii_lowercase();
    let source = match normalized.as_str() {
        "official" => "official",
        "github" => "github",
        "skills.sh" | "skills-sh" => "skills.sh",
        "lobehub" => "lobehub",
        "clawhub" => "clawhub",
        "claude-marketplace" => "claude-marketplace",
        _ => return None,
    };
    let key = rest.trim();
    if key.is_empty() {
        return None;
    }
    Some((source.to_string(), key.to_string()))
}

pub(crate) fn parse_repo_skill_identifier(identifier: &str) -> Option<(String, String)> {
    let trimmed = identifier.trim().trim_start_matches("github/");
    let pieces: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    if pieces.len() < 3 {
        return None;
    }
    let repo = format!("{}/{}", pieces[0], pieces[1]);
    let skill_dir = pieces[2..].join("/");
    if skill_dir.is_empty() {
        None
    } else {
        Some((repo, skill_dir))
    }
}

pub(crate) fn canonicalize_skills_sh_identifier(identifier: &str) -> String {
    identifier
        .trim()
        .trim_start_matches("skills.sh/")
        .trim_start_matches("skills-sh/")
        .to_string()
}
