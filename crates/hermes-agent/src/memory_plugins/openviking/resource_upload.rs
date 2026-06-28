fn is_remote_resource_source(value: &str) -> bool {
    REMOTE_RESOURCE_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn is_local_path_reference(value: &str) -> bool {
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return false;
    }
    if is_remote_resource_source(value) {
        return false;
    }
    is_windows_absolute_path(value)
        || value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with("~\\")
        || value.contains('/')
        || value.contains('\\')
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let Some(rest) = uri.strip_prefix("file://") else {
        return Err(format!("Unsupported file URI: {uri}"));
    };
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return Err(format!("Unsupported non-local file URI: {uri}"));
    };
    percent_decode_path(&path).map(PathBuf::from)
}

fn percent_decode_path(raw: &str) -> Result<String, String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return Err(format!("Invalid percent escape in path: {raw}"));
            }
            let hex = std::str::from_utf8(&bytes[idx + 1..idx + 3])
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            out.push(value);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("Invalid UTF-8 in path URI: {raw}"))
}

fn zip_directory(dir_path: &Path) -> Result<PathBuf, String> {
    let zip_path = std::env::temp_dir().join(format!(
        "openviking_upload_{}.zip",
        uuid::Uuid::new_v4().simple()
    ));
    let file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("create {}: {e}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_directory_to_zip(dir_path, dir_path, &mut zip, options)?;
    zip.finish()
        .map_err(|e| format!("finish {}: {e}", zip_path.display()))?;
    Ok(zip_path)
}

fn add_directory_to_zip(
    root: &Path,
    current: &Path,
    zip: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    for entry in
        std::fs::read_dir(current).map_err(|e| format!("read_dir {}: {e}", current.display()))?
    {
        let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", current.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| format!("metadata {}: {e}", path.display()))?;
        if metadata.is_dir() {
            add_directory_to_zip(root, &path, zip, options)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(rel, options)
            .map_err(|e| format!("zip start_file {}: {e}", path.display()))?;
        let mut file =
            std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        zip.write_all(&buffer)
            .map_err(|e| format!("zip write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn upload_temp_file(st: &VikingState, file_path: &Path) -> Result<String, String> {
    let file_name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("upload.bin")
        .to_string();
    let bytes =
        std::fs::read(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;
    let part = Part::bytes(bytes).file_name(file_name);
    let form = Form::new().part("file", part);
    let url = format!("{}/api/v1/resources/temp_upload", st.endpoint);
    let resp = st
        .client
        .post(&url)
        .headers(viking_multipart_headers(st))
        .multipart(form)
        .send()
        .map_err(|e| format!("OpenViking temp_upload failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("OpenViking temp_upload HTTP {status}: {text}"));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("OpenViking temp_upload JSON: {e}"))?;
    value
        .get("result")
        .and_then(|result| result.get("temp_file_id"))
        .or_else(|| value.get("temp_file_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "OpenViking temp_upload did not return temp_file_id".to_string())
}

fn add_resource_payload_for_source(
    source: &str,
    args: &Value,
) -> Result<(Value, Option<PathBuf>), String> {
    if args
        .get("to")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        && args
            .get("parent")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Err("Cannot specify both 'to' and 'parent'".to_string());
    }

    let mut body = json!({});
    for key in ["reason", "to", "parent", "instruction"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                body[key] = json!(value);
            }
        }
    }
    for key in ["wait", "timeout"] {
        if let Some(value) = args.get(key) {
            if !value.is_null() {
                body[key] = value.clone();
            }
        }
    }

    let source = source.trim();
    if is_remote_resource_source(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    }

    let path = if source.starts_with("file://") {
        file_uri_to_path(source)?
    } else if source.contains("://") && !is_windows_absolute_path(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    } else {
        PathBuf::from(source).expanduser()
    };

    if !path.exists() {
        if is_local_path_reference(source) {
            return Err(format!("Local resource path does not exist: {source}"));
        }
        body["path"] = json!(source);
        return Ok((body, None));
    }

    if path
        .symlink_metadata()
        .map_err(|e| format!("metadata {}: {e}", path.display()))?
        .file_type()
        .is_symlink()
    {
        return Err(format!(
            "Local resource path is a symlink and will not be uploaded: {source}"
        ));
    }

    if path.is_file() {
        body["source_name"] = json!(path.file_name().and_then(|v| v.to_str()).unwrap_or("file"));
        Ok((body, Some(path)))
    } else if path.is_dir() {
        body["source_name"] = json!(path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("directory"));
        Ok((body, Some(zip_directory(&path)?)))
    } else {
        Err(format!("Unsupported local resource path: {source}"))
    }
}

trait ExpandUserPath {
    fn expanduser(self) -> PathBuf;
}

impl ExpandUserPath for PathBuf {
    fn expanduser(self) -> PathBuf {
        let raw = self.to_string_lossy();
        if raw == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
        if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        self
    }
}
