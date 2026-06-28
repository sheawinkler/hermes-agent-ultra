async fn resolve_skill_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    requested_skill: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let mut suggestions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir
                .split('/')
                .next_back()
                .unwrap_or(skill_dir)
                .to_string();
            if skill_name.eq_ignore_ascii_case(requested_skill) {
                return Ok(ResolvedSkillSource {
                    repo: spec.repo.clone(),
                    branch,
                    skill_dir: skill_dir.to_string(),
                });
            }
            if skill_name
                .to_ascii_lowercase()
                .contains(&requested_skill.to_ascii_lowercase())
            {
                suggestions.insert(skill_name);
            }
        }
    }

    let suggestion_text = if suggestions.is_empty() {
        "none".to_string()
    } else {
        suggestions
            .into_iter()
            .take(8)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(AgentError::Config(format!(
        "Skill '{}' not found in configured taps. Suggestions: {}",
        requested_skill, suggestion_text
    )))
}

async fn resolve_skill_in_repo(
    client: &reqwest::Client,
    repo: &str,
    requested_skill: &str,
    preferred_prefix: Option<&str>,
) -> Result<ResolvedSkillSource, AgentError> {
    let branch = github_default_branch(client, repo).await?;
    let tree = github_repo_tree(client, repo, &branch).await?;

    let preferred_prefix = preferred_prefix
        .map(|v| v.trim_matches('/').to_string())
        .unwrap_or_default();
    let mut exact_candidates: Vec<String> = Vec::new();
    let mut fuzzy_candidates: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
            continue;
        }
        let skill_dir = entry.path.trim_end_matches("/SKILL.md").to_string();
        let skill_name = skill_dir
            .split('/')
            .next_back()
            .unwrap_or(skill_dir.as_str())
            .to_string();
        if skill_name.eq_ignore_ascii_case(requested_skill) {
            exact_candidates.push(skill_dir.clone());
        } else if skill_name
            .to_ascii_lowercase()
            .contains(&requested_skill.to_ascii_lowercase())
        {
            fuzzy_candidates.insert(skill_name);
        }
    }

    if exact_candidates.is_empty() {
        let suggestion_text = if fuzzy_candidates.is_empty() {
            "none".to_string()
        } else {
            fuzzy_candidates
                .into_iter()
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(AgentError::Config(format!(
            "Skill '{}' not found in repo {}. Suggestions: {}",
            requested_skill, repo, suggestion_text
        )));
    }

    exact_candidates.sort_by_key(|candidate| {
        let preferred = if preferred_prefix.is_empty() {
            1usize
        } else if candidate.starts_with(&format!("{}/", preferred_prefix)) {
            0usize
        } else {
            1usize
        };
        (preferred, candidate.len(), candidate.clone())
    });
    let skill_dir = exact_candidates
        .into_iter()
        .next()
        .ok_or_else(|| AgentError::Config("No matching skill path found.".into()))?;

    Ok(ResolvedSkillSource {
        repo: repo.to_string(),
        branch,
        skill_dir,
    })
}

async fn search_skills_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let query_l = query.to_ascii_lowercase();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();

    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir.split('/').next_back().unwrap_or(skill_dir);
            if !skill_name.to_ascii_lowercase().contains(&query_l) {
                continue;
            }
            let key = format!("{}/{}", spec.repo, skill_dir);
            if seen.insert(key.clone()) {
                out.push((skill_name.to_string(), key));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }

    Ok(out)
}

async fn fetch_skill_files_from_github(
    client: &reqwest::Client,
    source: &ResolvedSkillSource,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let tree = github_repo_tree(client, &source.repo, &source.branch).await?;
    let prefix = format!("{}/", source.skill_dir.trim_matches('/'));
    let mut files = Vec::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.starts_with(&prefix) {
            continue;
        }
        let rel_path = entry.path[prefix.len()..].to_string();
        ensure_safe_relative_path(&rel_path)?;
        let raw_path = entry
            .path
            .split('/')
            .map(|segment| urlencoding::encode(segment).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            source.repo, source.branch, raw_path
        );
        let bytes = match client
            .get(&raw_url)
            .header("User-Agent", "hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .bytes()
                .await
                .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?,
            _ => {
                let encoded_path = entry
                    .path
                    .split('/')
                    .map(urlencoding::encode)
                    .collect::<Vec<_>>()
                    .join("/");
                let api_url = format!(
                    "{}/repos/{}/contents/{}?ref={}",
                    GITHUB_API_BASE,
                    source.repo,
                    encoded_path,
                    urlencoding::encode(&source.branch)
                );
                let resp = github_request(client, &api_url, "application/vnd.github.v3.raw")
                    .timeout(std::time::Duration::from_secs(25))
                    .send()
                    .await
                    .map_err(|e| {
                        AgentError::Config(format!("GitHub file download failed: {}", e))
                    })?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AgentError::Config(format!(
                        "Failed to download {} from {} ({}): {}",
                        rel_path, source.repo, status, body
                    )));
                }
                resp.bytes()
                    .await
                    .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?
            }
        };
        files.push((rel_path, bytes));
    }

    if !files.iter().any(|(path, _)| path == "SKILL.md") {
        return Err(AgentError::Config(format!(
            "Resolved source {}/{} is missing SKILL.md",
            source.repo, source.skill_dir
        )));
    }
    if files.is_empty() {
        return Err(AgentError::Config(format!(
            "No files found at {}/{}",
            source.repo, source.skill_dir
        )));
    }
    Ok(files)
}

async fn fetch_lobehub_skill_files(
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

fn detect_archive_format(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 4
        && bytes[0] == 0x50
        && bytes[1] == 0x4B
        && bytes[2] == 0x03
        && bytes[3] == 0x04
    {
        return "zip";
    }
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        return "tar.gz";
    }
    "unknown"
}

fn extract_clawhub_archive(bytes: &[u8]) -> Result<Vec<(String, Bytes)>, AgentError> {
    match detect_archive_format(bytes) {
        "zip" => {
            let cursor = std::io::Cursor::new(bytes);
            let mut zip = zip::ZipArchive::new(cursor).map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub zip payload: {}", e))
            })?;
            let mut out = Vec::new();
            for i in 0..zip.len() {
                let mut file = zip.by_index(i).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub zip entry: {}", e))
                })?;
                if file.is_dir() {
                    continue;
                }
                let raw_name = file.name().replace('\\', "/");
                let segments: Vec<&str> = raw_name.split('/').filter(|s| !s.is_empty()).collect();
                let normalized = if segments.is_empty() {
                    file.name().to_string()
                } else if segments.len() == 1 {
                    segments[0].to_string()
                } else {
                    // Drop top-level archive folder if present.
                    segments[1..].join("/")
                };
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub file payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        "tar.gz" => {
            let decoder = flate2::read::GzDecoder::new(bytes);
            let mut archive = tar::Archive::new(decoder);
            let mut out = Vec::new();
            let entries = archive.entries().map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub tar payload: {}", e))
            })?;
            for entry in entries {
                let mut entry = entry.map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar entry: {}", e))
                })?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry
                    .path()
                    .map_err(|e| AgentError::Config(format!("Invalid tar entry path: {}", e)))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let normalized = path.split('/').skip(1).collect::<Vec<_>>().join("/");
                if normalized.is_empty() {
                    continue;
                }
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        _ => Err(AgentError::Config(
            "Unsupported ClawHub archive format (expected zip or tar.gz).".to_string(),
        )),
    }
}

async fn fetch_clawhub_skill_files(
    client: &reqwest::Client,
    slug: &str,
    version_hint: Option<&str>,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let detail_url = format!("{}/skills/{}", CLAWHUB_API_BASE, slug);
    let detail = client
        .get(&detail_url)
        .header("Accept", "application/json")
        .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("ClawHub detail request failed: {}", e)))?;
    if !detail.status().is_success() {
        let status = detail.status();
        let body = detail.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub detail lookup failed for '{}' ({}): {}",
            slug, status, body
        )));
    }
    let payload = detail
        .json::<ClawHubSkillDetailResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid ClawHub detail payload: {}", e)))?;
    let version = version_hint
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.to_string())
        .or_else(|| {
            let v = payload.latest_version.version.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        })
        .ok_or_else(|| {
            AgentError::Config(format!("No ClawHub version available for '{}'.", slug))
        })?;

    let download_url = format!(
        "{}/download?slug={}&version={}",
        CLAWHUB_API_BASE,
        urlencoding::encode(slug),
        urlencoding::encode(&version)
    );
    let mut last_err = String::new();
    for attempt in 1..=4 {
        let resp = client
            .get(&download_url)
            .header("Accept", "*/*")
            .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await
            .map_err(|e| AgentError::Config(format!("ClawHub download request failed: {}", e)))?;
        if resp.status().is_success() {
            let bytes = resp.bytes().await.map_err(|e| {
                AgentError::Config(format!("Invalid ClawHub download payload: {}", e))
            })?;
            return extract_clawhub_archive(&bytes);
        }
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait_secs = attempt * 2;
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs as u64)).await;
            last_err = "rate limited (429)".to_string();
            continue;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub download failed for '{}@{}' ({}): {}",
            slug, version, status, body
        )));
    }
    Err(AgentError::Config(format!(
        "ClawHub download for '{}@{}' failed after retries: {}",
        slug, version, last_err
    )))
}

