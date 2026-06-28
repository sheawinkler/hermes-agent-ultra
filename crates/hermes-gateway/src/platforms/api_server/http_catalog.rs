fn image_marker_message(image_url: &str, caption: Option<&str>) -> String {
    let mut marker = format!("[image] {image_url}");
    if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
        marker.push_str(&format!(" | caption={cap}"));
    }
    marker
}

// ---------------------------------------------------------------------------
// Connection handler (minimal HTTP/1.1 without axum dep for compilation)
// ---------------------------------------------------------------------------

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Content-Security-Policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    (
        "Permissions-Policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    (
        "Strict-Transport-Security",
        "max-age=31536000; includeSubDomains",
    ),
    ("X-Frame-Options", "DENY"),
    ("X-XSS-Protection", "0"),
];

#[derive(Clone, Copy)]
struct HttpStatus {
    code: u16,
    reason: &'static str,
}

const HTTP_OK: HttpStatus = HttpStatus {
    code: 200,
    reason: "OK",
};
const HTTP_ACCEPTED: HttpStatus = HttpStatus {
    code: 202,
    reason: "Accepted",
};
const HTTP_BAD_REQUEST: HttpStatus = HttpStatus {
    code: 400,
    reason: "Bad Request",
};
const HTTP_UNAUTHORIZED: HttpStatus = HttpStatus {
    code: 401,
    reason: "Unauthorized",
};
const HTTP_NOT_FOUND: HttpStatus = HttpStatus {
    code: 404,
    reason: "Not Found",
};
const HTTP_CONFLICT: HttpStatus = HttpStatus {
    code: 409,
    reason: "Conflict",
};
const HTTP_BAD_GATEWAY: HttpStatus = HttpStatus {
    code: 502,
    reason: "Bad Gateway",
};
const HTTP_SERVICE_UNAVAILABLE: HttpStatus = HttpStatus {
    code: 503,
    reason: "Service Unavailable",
};
const HTTP_GATEWAY_TIMEOUT: HttpStatus = HttpStatus {
    code: 504,
    reason: "Gateway Timeout",
};

fn append_security_headers(response: &mut String) {
    for (name, value) in SECURITY_HEADERS {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
}

fn http_response(status: HttpStatus, content_type: &str, body: &str) -> String {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n",
        status.code, status.reason, content_type
    );
    append_security_headers(&mut response);
    response.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
    response
}

fn json_http_response(status: HttpStatus, body: &serde_json::Value) -> serde_json::Result<String> {
    let payload = serde_json::to_string(body)?;
    Ok(http_response(status, "application/json", &payload))
}

fn api_error(message: impl Into<String>, error_type: &str, code: u16) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": message.into(),
            "type": error_type,
            "code": code.to_string(),
        }
    })
}

fn sse_http_header() -> String {
    let mut response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n".to_string();
    append_security_headers(&mut response);
    response.push_str("\r\n");
    response
}

fn split_path_query(raw_path: &str) -> (&str, Option<&str>) {
    raw_path
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((raw_path, None))
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (name == key).then(|| urlencoding::decode(value).unwrap_or_default().to_string())
    })
}

fn models_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "list",
        "data": [
            {
                "id": "hermes-agent",
                "object": "model",
                "owned_by": "hermes",
            },
            {
                "id": "hermes",
                "object": "model",
                "owned_by": "hermes",
            }
        ]
    })
}

fn capabilities_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "hermes.api_server.capabilities",
        "features": {
            "chat_completions": true,
            "responses": true,
            "response_store": true,
            "runs": true,
            "cron_jobs": true,
            "conversation_mapping": true,
            "session_continuity_header": "X-Hermes-Session-Id",
            "toolsets": true,
            "skills": true,
        },
        "endpoints": {
            "health": {"method": "GET", "path": "/health"},
            "models": {"method": "GET", "path": "/v1/models"},
            "chat_completions": {"method": "POST", "path": "/v1/chat/completions"},
            "responses": {"method": "POST", "path": "/v1/responses"},
            "response_get": {"method": "GET", "path": "/v1/responses/{response_id}"},
            "response_delete": {"method": "DELETE", "path": "/v1/responses/{response_id}"},
            "run_start": {"method": "POST", "path": "/v1/runs"},
            "run_status": {"method": "GET", "path": "/v1/runs/{run_id}"},
            "run_events": {"method": "GET", "path": "/v1/runs/{run_id}/events"},
            "run_approval": {"method": "POST", "path": "/v1/runs/{run_id}/approval"},
            "run_stop": {"method": "POST", "path": "/v1/runs/{run_id}/stop"},
            "jobs_list": {"method": "GET", "path": "/api/jobs"},
            "jobs_create": {"method": "POST", "path": "/api/jobs"},
            "jobs_get": {"method": "GET", "path": "/api/jobs/{job_id}"},
            "jobs_update": {"method": "PATCH", "path": "/api/jobs/{job_id}"},
            "jobs_delete": {"method": "DELETE", "path": "/api/jobs/{job_id}"},
            "jobs_pause": {"method": "POST", "path": "/api/jobs/{job_id}/pause"},
            "jobs_resume": {"method": "POST", "path": "/api/jobs/{job_id}/resume"},
            "jobs_run": {"method": "POST", "path": "/api/jobs/{job_id}/run"},
            "cron_fire": {"method": "POST", "path": "/api/cron/fire"},
            "skills": {"method": "GET", "path": "/v1/skills"},
            "toolsets": {"method": "GET", "path": "/v1/toolsets"},
        }
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SkillListEntry {
    name: String,
    description: String,
    category: String,
}

fn discover_skill_entries() -> Vec<SkillListEntry> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("skills"));
        roots.push(cwd.join("optional-skills"));
    }
    roots.push(hermes_config::paths::skills_dir());

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        collect_skill_entries(&root, &root, &mut seen, &mut out);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn collect_skill_entries(
    root: &Path,
    dir: &Path,
    seen: &mut HashSet<String>,
    out: &mut Vec<SkillListEntry>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if skill_md.is_file() {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_string();
            if seen.insert(name.clone()) {
                let category = path
                    .parent()
                    .and_then(|parent| parent.strip_prefix(root).ok())
                    .and_then(|relative| relative.components().next())
                    .and_then(|component| component.as_os_str().to_str())
                    .filter(|value| !value.is_empty())
                    .unwrap_or("general")
                    .to_string();
                out.push(SkillListEntry {
                    name,
                    description: skill_description(&skill_md),
                    category,
                });
            }
        } else {
            collect_skill_entries(root, &path, seen, out);
        }
    }
}

fn skill_description(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(240)
        .collect()
}

fn skills_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "list",
        "data": discover_skill_entries(),
    })
}

fn toolsets_response_body() -> serde_json::Value {
    let registry = Arc::new(ToolRegistry::new());
    let manager = ToolsetManager::new(registry);
    let default_api_toolset = "hermes-api-server";
    let data: Vec<serde_json::Value> = manager
        .list_toolsets()
        .into_iter()
        .map(|name| {
            let tools = manager
                .resolve_toolset_unfiltered(&name)
                .unwrap_or_else(|_| Vec::new());
            serde_json::json!({
                "name": name,
                "title": name.replace('-', " "),
                "description": "Built-in Hermes toolset",
                "enabled": name == default_api_toolset,
                "configured": name == default_api_toolset,
                "tools": tools,
            })
        })
        .collect();

    serde_json::json!({
        "object": "list",
        "platform": "api_server",
        "data": data,
    })
}

