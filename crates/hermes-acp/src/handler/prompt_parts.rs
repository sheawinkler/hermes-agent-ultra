fn canonical_mime(mime: Option<&str>) -> Option<String> {
    mime.map(|m| {
        m.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    })
    .filter(|m| !m.is_empty())
}

fn is_text_resource_mime(mime: Option<&str>) -> bool {
    let Some(mime) = canonical_mime(mime) else {
        return false;
    };
    TEXT_RESOURCE_MIME_PREFIXES
        .iter()
        .any(|prefix| mime.starts_with(prefix))
        || TEXT_RESOURCE_MIME_TYPES.contains(&mime.as_str())
}

fn is_image_resource_mime(mime: Option<&str>) -> bool {
    canonical_mime(mime)
        .map(|m| m.starts_with("image/"))
        .unwrap_or(false)
}

fn guess_image_mime_from_path(path: &Path) -> Option<&'static str> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    IMAGE_EXT_MIME
        .iter()
        .find_map(|(ext, mime)| lower.ends_with(ext).then_some(*mime))
}

fn read_resource_prefix(path: &Path, max_bytes: usize) -> Result<(Vec<u8>, usize), std::io::Error> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    let mut take = (&mut file).take(max_bytes as u64);
    take.read_to_end(&mut buf)?;
    let size = file.metadata()?.len() as usize;
    Ok((buf, size))
}

fn decode_text_bytes(data: &[u8], mime: Option<&str>) -> Option<String> {
    if data.contains(&0) && !is_text_resource_mime(mime) {
        return None;
    }
    if let Ok(text) = String::from_utf8(data.to_vec()) {
        return Some(text);
    }
    Some(String::from_utf8_lossy(data).into_owned())
}

fn resource_display_name(uri: &str, name: Option<&str>, title: Option<&str>) -> String {
    let name = name.unwrap_or("").trim();
    let title = title.unwrap_or("").trim();
    if !title.is_empty() && !name.is_empty() && title != name {
        return format!("{title} ({name})");
    }
    if !title.is_empty() {
        return title.to_string();
    }
    if !name.is_empty() {
        return name.to_string();
    }
    path_from_file_uri(uri)
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uri.to_string())
}

fn format_resource_text(
    uri: &str,
    body: &str,
    name: Option<&str>,
    title: Option<&str>,
    note: Option<&str>,
) -> String {
    let mut header = format!(
        "[Attached file: {}]",
        resource_display_name(uri, name, title)
    );
    if let Some(note) = note.filter(|n| !n.is_empty()) {
        header.push_str(&format!(" ({note})"));
    }
    if uri.trim().is_empty() {
        format!("{header}\n\n{body}")
    } else {
        format!("{header}\nURI: {uri}\n\n{body}")
    }
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.trim();
    if raw.is_empty() {
        return None;
    }
    if !raw.contains("://") {
        return Some(PathBuf::from(raw));
    }
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    if let Some(host) = parsed.host_str() {
        if host != "localhost" && !host.is_empty() {
            return None;
        }
    }
    let mut path_text = parsed.path().to_string();
    if path_text.starts_with("/%3A") {
        path_text = path_text.replacen("/%3A", ":", 1);
    }
    if path_text.len() >= 3 {
        let bytes = path_text.as_bytes();
        if bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
            let drive = (bytes[1] as char).to_ascii_lowercase();
            let rest = path_text[3..]
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/");
            return Some(PathBuf::from(format!("/mnt/{drive}/{rest}")));
        }
    }
    if path_text.len() >= 2 {
        let bytes = path_text.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = path_text[2..]
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/");
            return Some(PathBuf::from(format!("/mnt/{drive}/{rest}")));
        }
    }
    Some(PathBuf::from(path_text))
}

fn build_image_data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes))
}

fn json_text_part(text: impl Into<String>) -> Value {
    json!({
        "type": "text",
        "text": text.into(),
    })
}

fn json_image_part(url: impl Into<String>) -> Value {
    json!({
        "type": "image_url",
        "image_url": {
            "url": url.into(),
        }
    })
}

fn image_block_to_parts(block: &serde_json::Map<String, Value>) -> Vec<Value> {
    let mime = block
        .get("mimeType")
        .or_else(|| block.get("mime_type"))
        .and_then(|v| v.as_str());
    let image_mime = canonical_mime(mime).unwrap_or_else(|| "image/png".to_string());

    let data = block
        .get("data")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !data.is_empty() {
        let url = if data.starts_with("data:") {
            data.to_string()
        } else {
            format!("data:{image_mime};base64,{data}")
        };
        return vec![
            json_text_part(format!("[Attached image: {image_mime}]")),
            json_image_part(url),
        ];
    }

    let url = block
        .get("url")
        .and_then(|v| v.as_str())
        .or_else(|| block.get("image_url").and_then(|v| v.as_str()))
        .or_else(|| {
            block
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .trim();
    if url.is_empty() {
        Vec::new()
    } else {
        vec![
            json_text_part(format!("[Attached image]\nURL: {url}")),
            json_image_part(url),
        ]
    }
}

fn resource_link_to_parts(block: &serde_json::Map<String, Value>) -> Vec<Value> {
    let uri = block
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if uri.is_empty() {
        return Vec::new();
    }
    let name = block.get("name").and_then(|v| v.as_str());
    let title = block.get("title").and_then(|v| v.as_str());
    let mime = block
        .get("mimeType")
        .or_else(|| block.get("mime_type"))
        .and_then(|v| v.as_str());

    let Some(path) = path_from_file_uri(&uri) else {
        return vec![json_text_part(format_resource_text(
            &uri,
            "[Resource link only; Hermes cannot read non-file ACP resource URIs directly.]",
            name,
            title,
            None,
        ))];
    };

    let guessed_image_mime = if is_image_resource_mime(mime) {
        canonical_mime(mime)
    } else {
        guess_image_mime_from_path(&path).map(ToString::to_string)
    };
    if let Some(image_mime) = guessed_image_mime {
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                    return vec![json_text_part(format_resource_text(
                        &uri,
                        &format!(
                            "[Image too large to inline: {} bytes, cap={}]",
                            bytes.len(),
                            MAX_ACP_RESOURCE_BYTES
                        ),
                        name,
                        title,
                        None,
                    ))];
                }
                return vec![
                    json_text_part(format!(
                        "[Attached image: {}]\nURI: {}",
                        resource_display_name(&uri, name, title),
                        uri
                    )),
                    json_image_part(build_image_data_url(&image_mime, &bytes)),
                ];
            }
            Err(err) => {
                return vec![json_text_part(format_resource_text(
                    &uri,
                    &format!("[Could not read attached image: {err}]"),
                    name,
                    title,
                    None,
                ))];
            }
        }
    }

    match read_resource_prefix(&path, MAX_ACP_RESOURCE_BYTES) {
        Ok((bytes, size)) => {
            if let Some(text) = decode_text_bytes(&bytes, mime) {
                let note = if size > MAX_ACP_RESOURCE_BYTES {
                    Some(format!(
                        "truncated to {} of {} bytes",
                        MAX_ACP_RESOURCE_BYTES, size
                    ))
                } else {
                    None
                };
                vec![json_text_part(format_resource_text(
                    &uri,
                    &text,
                    name,
                    title,
                    note.as_deref(),
                ))]
            } else {
                vec![json_text_part(format_resource_text(
                    &uri,
                    &format!(
                        "[Binary file omitted: {} bytes, mime={}]",
                        size,
                        canonical_mime(mime).unwrap_or_else(|| "unknown".to_string())
                    ),
                    name,
                    title,
                    None,
                ))]
            }
        }
        Err(err) => vec![json_text_part(format_resource_text(
            &uri,
            &format!("[Could not read attached file: {err}]"),
            name,
            title,
            None,
        ))],
    }
}

fn embedded_resource_to_parts(block: &serde_json::Map<String, Value>) -> Vec<Value> {
    let resource = block
        .get("resource")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if resource.is_empty() {
        return Vec::new();
    }

    let uri = resource
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mime = resource
        .get("mimeType")
        .or_else(|| resource.get("mime_type"))
        .and_then(|v| v.as_str());

    if let Some(text) = resource.get("text").and_then(|v| v.as_str()) {
        return vec![json_text_part(format_resource_text(
            &uri, text, None, None, None,
        ))];
    }

    if let Some(blob) = resource.get("blob").and_then(|v| v.as_str()) {
        let bytes = BASE64_STANDARD
            .decode(blob)
            .unwrap_or_else(|_| blob.as_bytes().to_vec());
        if is_image_resource_mime(mime) {
            if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                return vec![json_text_part(format_resource_text(
                    &uri,
                    &format!(
                        "[Embedded image too large to inline: {} bytes, cap={}]",
                        bytes.len(),
                        MAX_ACP_RESOURCE_BYTES
                    ),
                    None,
                    None,
                    None,
                ))];
            }
            let image_mime = canonical_mime(mime).unwrap_or_else(|| "image/png".to_string());
            return vec![
                json_text_part(if uri.is_empty() {
                    format!(
                        "[Attached image: {}]",
                        resource_display_name("", None, None)
                    )
                } else {
                    format!(
                        "[Attached image: {}]\nURI: {}",
                        resource_display_name(&uri, None, None),
                        uri
                    )
                }),
                json_image_part(build_image_data_url(&image_mime, &bytes)),
            ];
        }

        if let Some(mut text) =
            decode_text_bytes(&bytes[..bytes.len().min(MAX_ACP_RESOURCE_BYTES)], mime)
        {
            if bytes.len() > MAX_ACP_RESOURCE_BYTES {
                text.push_str(&format!(
                    "\n\n[Truncated to {} of {} bytes]",
                    MAX_ACP_RESOURCE_BYTES,
                    bytes.len()
                ));
            }
            return vec![json_text_part(format_resource_text(
                &uri, &text, None, None, None,
            ))];
        }
        return vec![json_text_part(format_resource_text(
            &uri,
            &format!(
                "[Binary embedded file omitted: {} bytes, mime={}]",
                bytes.len(),
                canonical_mime(mime).unwrap_or_else(|| "unknown".to_string())
            ),
            None,
            None,
            None,
        ))];
    }

    Vec::new()
}

fn extract_prompt_payload(p: &serde_json::Map<String, Value>) -> PromptExtraction {
    if let Some(prompt_val) = p.get("prompt").or_else(|| p.get("content")) {
        if let Some(s) = prompt_val.as_str() {
            let text = s.to_string();
            return PromptExtraction {
                user_text: text.clone(),
                user_content: Value::String(text.clone()),
                text_only_prompt: true,
                has_content: !text.trim().is_empty(),
            };
        }
        if let Some(arr) = prompt_val.as_array() {
            let mut parts: Vec<Value> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            let mut text_only_prompt = true;

            for block in arr {
                let Some(obj) = block.as_object() else {
                    if let Some(text) = block.as_str() {
                        let text = text.to_string();
                        parts.push(json_text_part(text.clone()));
                        text_parts.push(text);
                    }
                    continue;
                };
                let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                match kind {
                    "text" => {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            let text = text.to_string();
                            parts.push(json_text_part(text.clone()));
                            text_parts.push(text);
                        }
                    }
                    "image" => {
                        text_only_prompt = false;
                        let image_parts = image_block_to_parts(obj);
                        for part in image_parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                            parts.push(part);
                        }
                    }
                    "resource_link" => {
                        text_only_prompt = false;
                        let resource_parts = resource_link_to_parts(obj);
                        for part in resource_parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                            parts.push(part);
                        }
                    }
                    "resource" => {
                        text_only_prompt = false;
                        let resource_parts = embedded_resource_to_parts(obj);
                        for part in resource_parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                text_parts.push(text.to_string());
                            }
                            parts.push(part);
                        }
                    }
                    _ => {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            let text = text.to_string();
                            parts.push(json_text_part(text.clone()));
                            text_parts.push(text);
                        }
                        if kind != "text" {
                            text_only_prompt = false;
                        }
                    }
                }
            }

            let user_text = text_parts.join("\n");
            let has_content = !parts.is_empty() || !user_text.trim().is_empty();
            return PromptExtraction {
                user_text,
                user_content: if parts.is_empty() {
                    Value::String(String::new())
                } else {
                    Value::Array(parts)
                },
                text_only_prompt,
                has_content,
            };
        }
    }

    let fallback = p
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    PromptExtraction {
        user_text: fallback.clone(),
        user_content: Value::String(fallback.clone()),
        text_only_prompt: true,
        has_content: !fallback.trim().is_empty(),
    }
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

struct SlashCommand {
    name: &'static str,
    description: &'static str,
    input_hint: Option<&'static str>,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        description: "Show available commands",
        input_hint: None,
    },
    SlashCommand {
        name: "model",
        description: "Show or change current model",
        input_hint: Some("model name to switch to"),
    },
    SlashCommand {
        name: "tools",
        description: "List available tools",
        input_hint: None,
    },
    SlashCommand {
        name: "context",
        description: "Show conversation context info",
        input_hint: None,
    },
    SlashCommand {
        name: "reset",
        description: "Clear conversation history",
        input_hint: None,
    },
    SlashCommand {
        name: "compact",
        description: "Compress conversation context",
        input_hint: None,
    },
    SlashCommand {
        name: "steer",
        description: "Inject guidance into the currently running agent turn",
        input_hint: Some("guidance for the active turn"),
    },
    SlashCommand {
        name: "queue",
        description: "Queue a prompt to run after the current turn finishes",
        input_hint: Some("prompt to run next"),
    },
    SlashCommand {
        name: "version",
        description: "Show Hermes version",
        input_hint: None,
    },
];
