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

#[derive(Debug, Deserialize)]
struct ClaudeMarketplaceManifest {
    #[serde(default)]
    plugins: Vec<ClaudeMarketplacePlugin>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeMarketplacePlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    skills: Vec<String>,
}

async fn fetch_claude_marketplace_manifest(
    client: &reqwest::Client,
) -> Result<ClaudeMarketplaceManifest, AgentError> {
    let url = format!(
        "{}/repos/anthropics/skills/contents/.claude-plugin/marketplace.json",
        GITHUB_API_BASE
    );
    let resp = github_request(client, &url, "application/vnd.github.v3.raw")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("Claude marketplace request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "Claude marketplace lookup failed ({}): {}",
            status, body
        )));
    }
    resp.json::<ClaudeMarketplaceManifest>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid Claude marketplace payload: {}", e)))
}

async fn resolve_claude_marketplace_skill(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let manifest = fetch_claude_marketplace_manifest(client).await?;
    let req = requested.trim().trim_matches('/').to_ascii_lowercase();
    let mut candidate_paths: Vec<String> = Vec::new();
    for plugin in manifest.plugins {
        let plugin_name = plugin.name.to_ascii_lowercase();
        for skill_path in plugin.skills {
            let normalized = skill_path
                .trim()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string();
            if normalized.is_empty() {
                continue;
            }
            let basename = normalized
                .split('/')
                .next_back()
                .unwrap_or(normalized.as_str())
                .to_ascii_lowercase();
            if req == basename
                || req == normalized.to_ascii_lowercase()
                || req == format!("{}/{}", plugin_name, basename)
                || req == format!("{}/{}", plugin_name, normalized.to_ascii_lowercase())
            {
                return Ok(ResolvedSkillSource {
                    repo: "anthropics/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: normalized,
                });
            }
            if basename.contains(&req) || normalized.to_ascii_lowercase().contains(&req) {
                candidate_paths.push(normalized);
            }
        }
    }
    candidate_paths.sort();
    candidate_paths.dedup();
    Err(AgentError::Config(format!(
        "Claude marketplace skill '{}' not found. Suggestions: {}",
        requested,
        if candidate_paths.is_empty() {
            "none".to_string()
        } else {
            candidate_paths
                .into_iter()
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        }
    )))
}

async fn resolve_official_skill_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let normalized = canonicalize_official_skill_dir(req.trim_start_matches("official/"));
    if normalized.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let branch = github_default_branch(client, OFFICIAL_SKILLS_REPO).await?;
    let tree = github_repo_tree(client, OFFICIAL_SKILLS_REPO, &branch).await?;
    let has_skill_dir = |dir: &str| -> bool {
        let target = format!("{}/SKILL.md", dir.trim_matches('/'));
        tree.iter()
            .any(|entry| entry.kind == "blob" && entry.path == target)
    };

    let mut candidate_queries = vec![
        req.to_string(),
        normalized.clone(),
        format!("official/{}", normalized),
    ];
    let basename = normalized
        .split('/')
        .next_back()
        .unwrap_or(normalized.as_str())
        .to_string();
    if !basename.is_empty() {
        candidate_queries.push(basename);
    }
    candidate_queries.sort();
    candidate_queries.dedup();

    for query in candidate_queries {
        if let Ok(record) = resolve_skill_via_registry_index(client, &query, Some("official")).await
        {
            if let RegistryInstallSource::GitRepo(source) = record.install_source {
                let mut candidates = official_skill_path_candidates(&source.skill_dir);
                for c in official_skill_path_candidates(&normalized) {
                    if !candidates.iter().any(|existing| existing == &c) {
                        candidates.push(c);
                    }
                }
                for candidate in candidates {
                    if has_skill_dir(&candidate) {
                        return Ok(ResolvedSkillSource {
                            repo: OFFICIAL_SKILLS_REPO.to_string(),
                            branch: branch.clone(),
                            skill_dir: candidate,
                        });
                    }
                }
            }
        }
    }

    for candidate in official_skill_path_candidates(&normalized) {
        if has_skill_dir(&candidate) {
            return Ok(ResolvedSkillSource {
                repo: OFFICIAL_SKILLS_REPO.to_string(),
                branch: branch.clone(),
                skill_dir: candidate,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Official skill '{}' not found in upstream skills or optional-skills catalogs.",
        requested
    )))
}

fn canonicalize_official_skill_dir(path: &str) -> String {
    path.trim().trim_matches('/').to_string()
}

fn official_skill_path_candidates(path_like: &str) -> Vec<String> {
    let normalized = canonicalize_official_skill_dir(path_like);
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.starts_with("skills/") || normalized.starts_with("optional-skills/") {
        return vec![normalized];
    }
    vec![
        format!("skills/{}", normalized),
        format!("optional-skills/{}", normalized),
    ]
}

async fn resolve_skills_sh_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing skills.sh skill identifier.".to_string(),
        ));
    }
    if let Some((repo, _, skill_dir)) = parse_explicit_github_skill(req) {
        let branch = github_default_branch(client, &repo).await?;
        return Ok(ResolvedSkillSource {
            repo,
            branch,
            skill_dir,
        });
    }

    if let Ok(resolved) = resolve_skill_via_registry_index(client, req, Some("skills.sh")).await {
        if let RegistryInstallSource::GitRepo(source) = resolved.install_source {
            let branch = github_default_branch(client, &source.repo).await?;
            return Ok(ResolvedSkillSource { branch, ..source });
        }
    }

    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", req), ("limit", "20")])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;
    let req_l = req.to_ascii_lowercase();
    for hit in payload.skills {
        let source = hit.source.trim();
        if source.is_empty() {
            continue;
        }
        let skill_id = if hit.skill_id.trim().is_empty() {
            hit.name.trim().to_string()
        } else {
            hit.skill_id.trim().to_string()
        };
        let repo = source.to_string();
        let branch = github_default_branch(client, &repo).await?;
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &skill_id, Some("skills")).await {
            return Ok(found);
        }
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &req_l, Some("skills")).await {
            return Ok(found);
        }
        if let Some((repo2, _, dir)) = parse_explicit_github_skill(&hit.id) {
            return Ok(ResolvedSkillSource {
                repo: repo2,
                branch,
                skill_dir: dir,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Unable to resolve skills.sh skill '{}'.",
        requested
    )))
}

async fn search_skills_sh_registry(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let capped_limit = limit.clamp(1, 50).to_string();
    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", query), ("limit", capped_limit.as_str())])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;

    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for hit in payload.skills {
        let id = hit.id.trim();
        if id.is_empty() {
            continue;
        }
        let identifier = format!("skills.sh/{}", id);
        if !seen.insert(identifier.clone()) {
            continue;
        }
        let display_name = if hit.name.trim().is_empty() {
            id.to_string()
        } else {
            hit.name.trim().to_string()
        };
        out.push((display_name, identifier));
    }
    Ok(out)
}

async fn resolve_install_via_fallback_router(
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

fn parse_repo_skill_identifier(identifier: &str) -> Option<(String, String)> {
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

fn canonicalize_skills_sh_identifier(identifier: &str) -> String {
    identifier
        .trim()
        .trim_start_matches("skills.sh/")
        .trim_start_matches("skills-sh/")
        .to_string()
}

async fn fetch_bundle_for_lock_entry(
    client: &reqwest::Client,
    entry: &SkillHubInstalledEntry,
    taps: &[String],
) -> Result<Vec<(String, Bytes)>, AgentError> {
    match entry.source.as_str() {
        "official" => {
            let resolved = resolve_official_skill_source(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "skills.sh" | "skills-sh" => {
            let id = canonicalize_skills_sh_identifier(&entry.identifier);
            let resolved = resolve_skills_sh_source(client, &id).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "lobehub" => fetch_lobehub_skill_files(client, &entry.identifier).await,
        "clawhub" => fetch_clawhub_skill_files(client, &entry.identifier, None).await,
        "claude-marketplace" => {
            let resolved = resolve_claude_marketplace_skill(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "tap" => {
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved = resolve_skill_via_taps(client, taps, &entry.name).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "github" => {
            if let Some((repo, maybe_branch, skill_dir)) =
                parse_explicit_github_skill(&entry.identifier)
            {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(client, &repo).await?
                };
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved =
                resolve_skill_in_repo(client, &entry.identifier, &entry.name, None).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        other => {
            if let Ok(hit) =
                resolve_skill_via_registry_index(client, &entry.identifier, Some(other)).await
            {
                return match hit.install_source {
                    RegistryInstallSource::GitRepo(source) => {
                        let branch = github_default_branch(client, &source.repo).await?;
                        fetch_skill_files_from_github(
                            client,
                            &ResolvedSkillSource { branch, ..source },
                        )
                        .await
                    }
                    RegistryInstallSource::LobeRegistry { slug } => {
                        fetch_lobehub_skill_files(client, &slug).await
                    }
                    RegistryInstallSource::ClawRegistry { slug, version } => {
                        fetch_clawhub_skill_files(client, &slug, version.as_deref()).await
                    }
                };
            }
            Err(AgentError::Config(format!(
                "Unknown hub source '{}' for installed skill '{}'",
                entry.source, entry.name
            )))
        }
    }
}

fn install_skill_files(
    skills_dir: &std::path::Path,
    install_name: &str,
    files: &[(String, Bytes)],
) -> Result<std::path::PathBuf, AgentError> {
    skill_guard_scan_bundle(files)?;

    std::fs::create_dir_all(skills_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create skills dir: {}", e)))?;

    let target = skills_dir.join(install_name);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| AgentError::Io(format!("Failed to remove existing skill dir: {}", e)))?;
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| AgentError::Io(format!("Failed to create skill dir: {}", e)))?;

    for (rel_path, bytes) in files {
        ensure_safe_relative_path(rel_path)?;
        let output = target.join(rel_path);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create parent dirs: {}", e)))?;
        }
        std::fs::write(&output, bytes)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", output.display(), e)))?;
    }

    let skill_md = target.join("SKILL.md");
    if !skill_md.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill is missing SKILL.md at {}",
            skill_md.display()
        )));
    }

    Ok(target)
}

fn skill_auto_bootstrap_enabled() -> bool {
    !std::env::var("HERMES_SKILL_AUTO_BOOTSTRAP")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
}

fn skill_bootstrap_force_confirmed() -> bool {
    std::env::var("HERMES_SKILL_BOOTSTRAP_ASSUME_YES")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        || std::env::var("HERMES_SKILL_BOOTSTRAP_FORCE")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
}

fn prompt_bootstrap_yes_no(prompt: &str, default_yes: bool) -> bool {
    use std::io::Write as _;
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return default_yes;
    }
    let answer = buf.trim().to_ascii_lowercase();
    if answer.is_empty() {
        return default_yes;
    }
    matches!(answer.as_str(), "y" | "yes")
}

fn push_bootstrap_command_if_present(commands: &mut Vec<String>, raw: Option<&str>) {
    if let Some(raw) = raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            commands.push(trimmed.to_string());
        }
    }
}

fn collect_bootstrap_commands_from_value(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => push_bootstrap_command_if_present(out, Some(s)),
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    push_bootstrap_command_if_present(out, Some(s));
                }
            }
        }
        serde_json::Value::Object(map) => {
            push_bootstrap_command_if_present(out, map.get("command").and_then(|v| v.as_str()));
            if let Some(commands) = map.get("commands") {
                collect_bootstrap_commands_from_value(commands, out);
            }
            if let Some(script) = map.get("script").and_then(|v| v.as_str()) {
                let script = script.trim();
                if !script.is_empty() {
                    if script.ends_with(".py") {
                        out.push(format!("python3 {}", script));
                    } else {
                        out.push(format!("bash {}", script));
                    }
                }
            }
            if let Some(scripts) = map.get("scripts").and_then(|v| v.as_array()) {
                for script in scripts {
                    if let Some(script) = script.as_str() {
                        let script = script.trim();
                        if script.is_empty() {
                            continue;
                        }
                        if script.ends_with(".py") {
                            out.push(format!("python3 {}", script));
                        } else {
                            out.push(format!("bash {}", script));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn parse_skill_bootstrap_plan(
    files: &[(String, Bytes)],
) -> Result<Option<SkillBootstrapPlan>, AgentError> {
    let skill_md = files
        .iter()
        .find_map(|(path, bytes)| {
            if path == "SKILL.md" {
                Some(bytes)
            } else {
                None
            }
        })
        .ok_or_else(|| AgentError::Config("Installed skill payload is missing SKILL.md".into()))?;

    let content = std::str::from_utf8(skill_md)
        .map_err(|e| AgentError::Config(format!("Installed SKILL.md is not valid UTF-8: {}", e)))?;
    let (frontmatter, _body) = hermes_tools::tools::skill_utils::parse_frontmatter(content);

    let mut commands = Vec::new();
    for key in [
        "bootstrap",
        "setup",
        "install",
        "bootstrap_command",
        "setup_command",
        "install_command",
        "bootstrap_commands",
        "setup_commands",
        "install_commands",
    ] {
        if let Some(value) = frontmatter.get(key) {
            collect_bootstrap_commands_from_value(value, &mut commands);
        }
    }

    let mut dedup = HashSet::new();
    let normalized: Vec<String> = commands
        .into_iter()
        .filter_map(|cmd| {
            let trimmed = cmd.trim().to_string();
            if trimmed.is_empty() || !dedup.insert(trimmed.clone()) {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect();

    if normalized.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SkillBootstrapPlan {
            commands: normalized,
        }))
    }
}

fn is_allowed_bootstrap_executable(executable: &str) -> bool {
    let normalized = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .trim()
        .to_ascii_lowercase();
    SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES
        .iter()
        .any(|allowed| *allowed == normalized)
}

fn parse_bootstrap_command(raw: &str) -> Result<ParsedBootstrapCommand, AgentError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command cannot be empty".to_string(),
        ));
    }
    if trimmed.len() > 2048 {
        return Err(AgentError::Config(
            "Bootstrap command is too long (>2048 bytes)".to_string(),
        ));
    }

    // Deliberately block shell control operators and substitutions.
    let forbidden = Regex::new(r"[`\n\r;]|&&|\|\||\||>>?|<<?|\$\(").expect("valid regex");
    if forbidden.is_match(trimmed) {
        return Err(AgentError::Config(format!(
            "Blocked bootstrap command (contains forbidden shell operators): {}",
            trimmed
        )));
    }

    let mut tokens = shlex::split(trimmed).ok_or_else(|| {
        AgentError::Config(format!(
            "Unable to parse bootstrap command safely: {}",
            trimmed
        ))
    })?;
    if tokens.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command parsed to no executable".to_string(),
        ));
    }

    let executable = tokens.remove(0);
    if executable.contains('/') || executable.contains('\\') {
        let path = Path::new(&executable);
        if path.is_absolute() {
            return Err(AgentError::Config(format!(
                "Bootstrap executable must be relative (got absolute path): {}",
                executable
            )));
        }
        ensure_safe_relative_path(&executable)?;
        if executable.ends_with(".sh") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "bash".to_string(),
                args,
            });
        }
        if executable.ends_with(".py") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "python3".to_string(),
                args,
            });
        }
    } else if !is_allowed_bootstrap_executable(&executable) {
        return Err(AgentError::Config(format!(
            "Bootstrap executable '{}' is not in the allowlist",
            executable
        )));
    }

    Ok(ParsedBootstrapCommand {
        display: trimmed.to_string(),
        executable,
        args: tokens,
    })
}

async fn execute_bootstrap_command(
    skill_dir: &Path,
    command: &ParsedBootstrapCommand,
) -> Result<(), AgentError> {
    let exec_path = if command.executable.contains('/') || command.executable.contains('\\') {
        skill_dir.join(&command.executable)
    } else {
        PathBuf::from(&command.executable)
    };

    let mut process = tokio::process::Command::new(&exec_path);
    process
        .args(&command.args)
        .current_dir(skill_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = process.output().await.map_err(|e| {
        AgentError::Io(format!(
            "Failed to execute bootstrap command '{}': {}",
            command.display, e
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        if !stdout.is_empty() {
            println!(
                "    stdout: {}",
                stdout.lines().take(3).collect::<Vec<_>>().join(" | ")
            );
        }
        Ok(())
    } else {
        Err(AgentError::Config(format!(
            "Bootstrap command failed (exit={}): {}\n{}\n{}",
            output.status,
            command.display,
            if stdout.is_empty() { "" } else { "stdout:" },
            if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}\nstderr:\n{}", stdout, stderr)
            }
        )))
    }
}

async fn maybe_run_skill_bootstrap(
    install_name: &str,
    skill_dir: &Path,
    files: &[(String, Bytes)],
) -> Result<(), AgentError> {
    if !skill_auto_bootstrap_enabled() {
        println!("Skill bootstrap skipped: HERMES_SKILL_AUTO_BOOTSTRAP=0.");
        return Ok(());
    }

    let Some(plan) = parse_skill_bootstrap_plan(files)? else {
        return Ok(());
    };

    let mut runnable: Vec<(ParsedBootstrapCommand, hermes_tools::ApprovalDecision)> = Vec::new();
    let mut blocked: Vec<(String, String)> = Vec::new();
    for raw in plan.commands {
        match parse_bootstrap_command(&raw) {
            Ok(parsed) => {
                let decision = hermes_tools::check_approval(&parsed.display);
                if matches!(decision, hermes_tools::ApprovalDecision::Denied) {
                    blocked.push((
                        parsed.display,
                        "blocked by command approval policy".to_string(),
                    ));
                } else {
                    runnable.push((parsed, decision));
                }
            }
            Err(err) => blocked.push((raw, err.to_string())),
        }
    }

    if runnable.is_empty() && blocked.is_empty() {
        return Ok(());
    }

    println!(
        "Detected bootstrap plan for '{}': {} runnable command(s), {} blocked.",
        install_name,
        runnable.len(),
        blocked.len()
    );
    for (cmd, reason) in &blocked {
        println!("  - blocked: `{}` ({})", cmd, reason);
    }
    if runnable.is_empty() {
        return Ok(());
    }

    let has_confirm = runnable.iter().any(|(_, decision)| {
        matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        )
    });
    let force_yes = skill_bootstrap_force_confirmed();
    if has_confirm && !force_yes {
        let proceed = prompt_bootstrap_yes_no(
            "Run bootstrap commands that require installer confirmation now? [Y/n]: ",
            true,
        );
        if !proceed {
            println!("Skipped bootstrap execution.");
            return Ok(());
        }
    }

    for (command, decision) in runnable {
        if matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        ) && !force_yes
        {
            println!("  • running (confirmed): {}", command.display);
        } else if matches!(decision, hermes_tools::ApprovalDecision::Approved) {
            println!("  • running: {}", command.display);
        } else if !force_yes {
            println!("  • skipped: {} (confirmation required)", command.display);
            continue;
        } else {
            println!("  • running (forced): {}", command.display);
        }
        execute_bootstrap_command(skill_dir, &command).await?;
    }

    Ok(())
}

fn normalize_tap_path_for_storage(path: &str) -> String {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized)
    }
}

fn tap_object_to_string(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if let Some(url) = obj
        .get("url")
        .and_then(|u| u.as_str())
        .or_else(|| obj.get("tap").and_then(|u| u.as_str()))
    {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let repo = obj.get("repo").and_then(|v| v.as_str())?;
    let repo = repo.trim().trim_matches('/');
    if repo.is_empty() {
        return None;
    }
    let path = obj
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("skills/")
        .trim()
        .trim_matches('/');
    if path.is_empty() {
        Some(format!("https://github.com/{}::", repo))
    } else {
        Some(format!("https://github.com/{}::{}", repo, path))
    }
}

fn tap_string_to_object(tap: &str) -> serde_json::Value {
    if let Some(spec) = parse_skill_tap_spec(tap) {
        let mut obj = serde_json::Map::new();
        obj.insert("repo".to_string(), serde_json::Value::String(spec.repo));
        obj.insert(
            "path".to_string(),
            serde_json::Value::String(normalize_tap_path_for_storage(&spec.path)),
        );
        obj.insert(
            "url".to_string(),
            serde_json::Value::String(tap.to_string()),
        );
        serde_json::Value::Object(obj)
    } else {
        serde_json::json!({ "url": tap })
    }
}

fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        serde_json::Value::Object(map) => {
            let taps = map.get("taps").cloned().unwrap_or(serde_json::Value::Null);
            match taps {
                serde_json::Value::Array(arr) => arr
                    .into_iter()
                    .filter_map(|item| match item {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Object(obj) => tap_object_to_string(&obj),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn subscription_entry_to_source(entry: &serde_json::Value) -> Option<String> {
    match entry {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Object(obj) => {
            let source = obj
                .get("source")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("tap").and_then(|v| v.as_str()))
                .or_else(|| obj.get("url").and_then(|v| v.as_str()))?;
            let trimmed = source.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    }
}

fn read_skill_subscriptions(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(subscription_entry_to_source)
            .collect(),
        serde_json::Value::Object(map) => {
            let subscriptions = map
                .get("subscriptions")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            match subscriptions {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(subscription_entry_to_source)
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn write_skill_taps(path: &std::path::Path, taps: &[String]) -> Result<(), AgentError> {
    let serialized_taps: Vec<serde_json::Value> =
        taps.iter().map(|tap| tap_string_to_object(tap)).collect();
    let payload = serde_json::json!({
        "taps": serialized_taps
    });
    let json =
        serde_json::to_string_pretty(&payload).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, format!("{}\n", json)).map_err(|e| AgentError::Io(e.to_string()))?;
    Ok(())
}

fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for tap in DEFAULT_SKILL_TAPS {
        merged.push((*tap).to_string());
    }
    for tap in custom_taps {
        if !merged.iter().any(|existing| existing == tap) {
            merged.push(tap.clone());
        }
    }
    merged
}

fn subscription_source_to_tap(source: &str) -> Option<String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://github.com/") || lower.starts_with("http://github.com/") {
        return parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string());
    }
    if lower.contains("://") {
        return None;
    }
    if let Some((prefix, _)) = trimmed.split_once('/') {
        let p = prefix.trim().to_ascii_lowercase();
        if matches!(
            p.as_str(),
            "official" | "skills.sh" | "lobehub" | "clawhub" | "claude-marketplace" | "github"
        ) {
            return None;
        }
    }
    parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string())
}

fn effective_skill_taps(
    taps_file: &std::path::Path,
    subscriptions_file: &std::path::Path,
) -> Vec<String> {
    let custom_taps = read_skill_taps(taps_file);
    let mut merged = merged_skill_taps(&custom_taps);
    for sub in read_skill_subscriptions(subscriptions_file) {
        // Subscriptions may include non-tap registries; only include values that
        // can be interpreted as GitHub tap specs.
        let Some(tap) = subscription_source_to_tap(&sub) else {
            continue;
        };
        if !merged.iter().any(|existing| existing == &tap) {
            merged.push(tap);
        }
    }
    merged
}

/// Return auto-completion suggestions for a partial slash command.
pub fn autocomplete(partial: &str) -> Vec<&'static str> {
    hermes_cli_ui::autocomplete(partial, SLASH_COMMANDS)
}

/// Return contextual auto-completion suggestions for slash commands.
///
/// Unlike [`autocomplete`], this understands command argument position and can
/// suggest nested values like `/swarm run <passes> <mode>`.
pub fn autocomplete_contextual(partial: &str) -> Vec<String> {
    autocomplete_contextual_with_runtime(partial, None)
}

pub fn autocomplete_contextual_for_app(partial: &str, app: &App) -> Vec<String> {
    autocomplete_contextual_with_runtime(
        partial,
        Some(CompletionRuntime {
            config: app.config.as_ref(),
            tool_registry: app.tool_registry.as_ref(),
        }),
    )
}

struct CompletionRuntime<'a> {
    config: &'a GatewayConfig,
    tool_registry: &'a hermes_tools::ToolRegistry,
}

fn autocomplete_contextual_with_runtime(
    partial: &str,
    runtime: Option<CompletionRuntime<'_>>,
) -> Vec<String> {
    let trimmed_start = partial.trim_start();
    if !trimmed_start.starts_with('/') {
        return Vec::new();
    }
    let trailing_space = trimmed_start
        .chars()
        .last()
        .is_some_and(char::is_whitespace);
    let tokens: Vec<&str> = trimmed_start.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    // First token only: preserve current fuzzy top-level behavior.
    if tokens.len() == 1 && !trailing_space {
        return autocomplete(trimmed_start)
            .into_iter()
            .map(ToString::to_string)
            .collect();
    }

    let Some(cmd) = resolve_completion_command(tokens[0]) else {
        return autocomplete(tokens[0])
            .into_iter()
            .map(ToString::to_string)
            .collect();
    };

    let args = if tokens.len() > 1 {
        tokens[1..].to_vec()
    } else {
        Vec::new()
    };

    let (arg_position, fragment) = if args.is_empty() {
        (0usize, "")
    } else if trailing_space {
        (args.len(), "")
    } else {
        (args.len() - 1, args[args.len() - 1])
    };

    let candidates = command_argument_candidates(&cmd, &args, arg_position, runtime.as_ref());

    if candidates.is_empty() {
        return Vec::new();
    }

    let fragment_lc = fragment.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for candidate in candidates {
        if !fragment_lc.is_empty() && !candidate.to_ascii_lowercase().starts_with(&fragment_lc) {
            continue;
        }
        let mut parts: Vec<String> = Vec::with_capacity(1 + arg_position + 1);
        parts.push(cmd.clone());
        for i in 0..arg_position {
            if i < args.len() {
                parts.push(args[i].to_string());
            }
        }
        parts.push(candidate.to_string());
        let mut suggestion = parts.join(" ");
        if trailing_space {
            suggestion.push(' ');
        }
        if seen.insert(suggestion.clone()) {
            out.push(suggestion);
        }
    }
    out
}

fn command_argument_candidates(
    cmd: &str,
    args: &[&str],
    arg_position: usize,
    runtime: Option<&CompletionRuntime<'_>>,
) -> Vec<String> {
    match (cmd, arg_position) {
        ("/personality", 0) => personality_completion_candidates(),
        ("/handoff", 0) => handoff_completion_candidates(runtime),
        ("/tools", 0) => ["list", "trust", "enable", "disable"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/tools", 1) => args
            .first()
            .map(|sub| tool_completion_candidates(runtime, sub))
            .unwrap_or_default(),
        _ if arg_position == 0 => command_subcommand_candidates(cmd),
        _ => command_nested_candidates(cmd, args[0], arg_position),
    }
}

fn personality_completion_candidates() -> Vec<String> {
    let mut out = vec!["list".to_string(), "none".to_string()];
    out.extend(
        hermes_agent::builtin_personality_names()
            .iter()
            .map(|v| (*v).to_string()),
    );
    out.sort();
    out.dedup();
    out
}

fn handoff_completion_candidates(runtime: Option<&CompletionRuntime<'_>>) -> Vec<String> {
    let Some(runtime) = runtime else {
        return Vec::new();
    };
    let mut out: Vec<String> = runtime
        .config
        .platforms
        .iter()
        .filter(|(_, platform)| platform.enabled)
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn tool_completion_candidates(
    runtime: Option<&CompletionRuntime<'_>>,
    subcommand: &str,
) -> Vec<String> {
    let Some(runtime) = runtime else {
        return Vec::new();
    };
    let action = subcommand.trim().to_ascii_lowercase();
    if action != "enable" && action != "disable" {
        return Vec::new();
    }

    let disabled: HashSet<&str> = runtime
        .config
        .tools_config
        .disabled
        .iter()
        .map(String::as_str)
        .collect();

    let mut out: Vec<String> = runtime
        .tool_registry
        .list_tools()
        .into_iter()
        .filter_map(|tool| {
            let active = !disabled.contains(tool.name.as_str());
            match (action.as_str(), active) {
                ("enable", false) | ("disable", true) => Some(tool.name),
                _ => None,
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn resolve_completion_command(raw: &str) -> Option<String> {
    let canonical = canonical_command(raw);
    if SLASH_COMMANDS.iter().any(|(name, _)| *name == canonical) {
        return Some(canonical.to_string());
    }
    let exact = autocomplete(raw);
    if exact.len() == 1 {
        return exact
            .first()
            .copied()
            .map(canonical_command)
            .map(ToString::to_string);
    }
    None
}

fn command_subcommand_candidates(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for value in command_subcommand_overrides(cmd) {
        if seen.insert(value.to_string()) {
            out.push(value.to_string());
        }
    }
    for value in inferred_subcommands_from_description(cmd) {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn command_nested_candidates(cmd: &str, subcommand: &str, arg_position: usize) -> Vec<String> {
    let sub = subcommand.to_ascii_lowercase();
    match (cmd, sub.as_str(), arg_position) {
        ("/swarm", "plan", 1) => ["concurrent", "sequential", "graph"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "run", 1) => ["1", "2", "4", "8", "16", "32", "64"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "run", 2) => ["concurrent", "sequential", "graph"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "voters", 1) => ["2", "3", "4", "5", "6", "7", "8"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/quorum", "voters", 1) => ["2", "3", "4", "5", "6", "7", "8"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "lifecycle", 1) => [
            "status",
            "active",
            "pause",
            "resume",
            "budget-limited",
            "achieved",
            "unmet",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        ("/objective", "behavior", 1) => [
            "status",
            "list",
            "balanced",
            "strict",
            "autonomous",
            "mission",
            "minimal",
            "sigma",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        ("/objective", "profile", 1) => ["status", "list", "general", "me", "set"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "context", 1) => ["status", "list", "max", "balanced", "fast"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "simulator", 1) => ["status", "balanced", "strict", "aggressive"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "ensemble", 1) => ["status", "committee", "single", "debate"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "ledger", 1) => ["status", "tail", "clear"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "dag", 1) => ["status", "rebuild", "clear"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "eval", 1) => ["status", "tail"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "wait", 1) => ["--session", "--seconds", "for"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/model", "why-not", 1) => [
            "--cap",
            "--min-context",
            "--max-input-cost",
            "--max-output-cost",
            "--budget",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        _ => Vec::new(),
    }
}

fn command_subcommand_overrides(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "/auth" => &["status", "verify", "refresh"],
        "/context" => &["status", "breakdown", "compress"],
        "/pet" => &[
            "status", "on", "off", "toggle", "list", "set", "mood", "dock", "speed",
        ],
        "/agents" => &["status", "pause", "resume", "doctor"],
        "/objective" => &[
            "status",
            "verify",
            "plan",
            "constraints",
            "counterfactual",
            "wait",
            "unwait",
            "profile",
            "context",
            "simulator",
            "ensemble",
            "ledger",
            "dag",
            "eval",
            "clear",
            "lifecycle",
            "behavior",
        ],
        "/quorum" => &["status", "on", "off", "voters", "models", "run"],
        "/swarm" => &[
            "status", "plan", "run", "cancel", "artifact", "on", "off", "voters", "models",
        ],
        "/simulate" => &["status"],
        "/timetravel" => &["list", "latest", "goto", "undo", "branch"],
        "/autocompact" => &["status", "now", "governance"],
        "/qos" => &["status", "health", "autotune"],
        "/claims" => &["status", "on", "off"],
        _ => &[],
    }
}

fn inferred_subcommands_from_description(cmd: &str) -> Vec<String> {
    let Some((_, desc)) = SLASH_COMMANDS.iter().find(|(name, _)| *name == cmd) else {
        return Vec::new();
    };
    let mut segments: Vec<String> = Vec::new();
    let mut in_tick = false;
    let mut buf = String::new();
    for ch in desc.chars() {
        if ch == '`' {
            if in_tick && !buf.trim().is_empty() {
                segments.push(buf.clone());
            }
            buf.clear();
            in_tick = !in_tick;
            continue;
        }
        if in_tick {
            buf.push(ch);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for seg in segments {
        for raw in seg.split('|') {
            let cleaned = raw
                .trim()
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .trim_start_matches('/');
            if cleaned.is_empty() {
                continue;
            }
            let lc = cleaned.to_ascii_lowercase();
            if lc == cmd.trim_start_matches('/') {
                continue;
            }
            if !lc
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                continue;
            }
            if seen.insert(lc.clone()) {
                out.push(lc);
            }
        }
    }
    out
}

/// Return the help text for a specific slash command.
pub fn help_for(cmd: &str) -> Option<&'static str> {
    hermes_cli_ui::help_for(cmd, SLASH_COMMANDS)
}

fn canonical_command(cmd: &str) -> &str {
    hermes_cli_ui::canonical_command(cmd)
}
