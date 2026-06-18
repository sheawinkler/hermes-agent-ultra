use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("no download mirrors configured for this OS/CPU")]
    NoMirrors,
    #[error("all mirrors failed reachability probe")]
    ProbeFailed,
    #[error("version resolution failed: {0}")]
    Version(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("extract failed: {0}")]
    Extract(String),
    #[error("command failed: {0}")]
    Command(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
    TarXz,
    TarGz,
}

pub fn http_client() -> Result<Client, InstallError> {
    Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("hermes-agent-ultra/dep-install")
        .build()
        .map_err(|e| InstallError::Download(e.to_string()))
}

pub async fn download_file(client: &Client, url: &str, dest: &Path) -> Result<(), InstallError> {
    let mut request = client.get(url);
    if url.contains("github.com") || url.contains("githubusercontent.com") {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            request = request
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/octet-stream");
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if !response.status().is_success() {
        return Err(InstallError::Download(format!(
            "HTTP {}",
            response.status()
        )));
    }

    let mut file = tokio::fs::File::create(dest).await?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| InstallError::Download(e.to_string()))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;
    }
    file.flush().await?;
    Ok(())
}

pub fn archive_filename(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("archive").to_string()
}

pub fn extract_binary(
    archive_path: &Path,
    format: ArchiveFormat,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    match format {
        ArchiveFormat::Zip => extract_from_zip(archive_path, dest, binary_name),
        ArchiveFormat::SevenZip => extract_from_7z(archive_path, dest, binary_name),
        ArchiveFormat::TarXz => extract_from_tar_xz(archive_path, dest, binary_name),
        ArchiveFormat::TarGz => extract_from_tar_gz(archive_path, dest, binary_name),
    }
}

pub fn extract_tree(
    archive_path: &Path,
    format: ArchiveFormat,
    dest: &Path,
) -> Result<(), InstallError> {
    let temp = archive_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("extract-{}", std::process::id()));
    if temp.exists() {
        std::fs::remove_dir_all(&temp)?;
    }
    std::fs::create_dir_all(&temp)?;

    match format {
        ArchiveFormat::Zip => extract_zip_tree(archive_path, &temp)?,
        ArchiveFormat::TarXz => extract_tar_tree(archive_path, &temp, true)?,
        ArchiveFormat::TarGz => extract_tar_tree(archive_path, &temp, false)?,
        ArchiveFormat::SevenZip => {
            sevenz_rust::decompress_file(archive_path, &temp)
                .map_err(|e| InstallError::Extract(e.to_string()))?;
        }
    }

    let payload = single_payload_dir(&temp)?;
    copy_dir_merge(&payload, dest)?;
    let _ = std::fs::remove_dir_all(&temp);
    Ok(())
}

pub fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

pub fn find_file_named(root: &Path, name: &str, max_depth: u32) -> Result<PathBuf, InstallError> {
    fn walk(
        dir: &Path,
        name: &str,
        depth: u32,
        max_depth: u32,
    ) -> Result<Option<PathBuf>, InstallError> {
        if depth > max_depth {
            return Ok(None);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Ok(Some(path));
            }
            if path.is_dir() {
                if let Some(found) = walk(&path, name, depth + 1, max_depth)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    walk(root, name, 0, max_depth)?
        .ok_or_else(|| InstallError::Extract(format!("{name} not found under {}", root.display())))
}

fn extract_from_zip(
    archive_path: &Path,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| InstallError::Extract(e.to_string()))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| InstallError::Extract(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with(binary_name) || name == binary_name {
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(InstallError::Extract(format!(
        "{binary_name} not found in zip"
    )))
}

fn extract_from_7z(
    archive_path: &Path,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    let extract_dir = archive_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("7z-extract");
    if extract_dir.exists() {
        std::fs::remove_dir_all(&extract_dir)?;
    }
    std::fs::create_dir_all(&extract_dir)?;
    sevenz_rust::decompress_file(archive_path, &extract_dir)
        .map_err(|e| InstallError::Extract(e.to_string()))?;
    let found = find_file_named(&extract_dir, binary_name, 6)?;
    std::fs::copy(&found, dest)?;
    let _ = std::fs::remove_dir_all(&extract_dir);
    Ok(())
}

fn extract_from_tar_xz(
    archive_path: &Path,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive_path)?;
    let decompressor = xz2::read::XzDecoder::new(file);
    extract_named_from_tar(tar::Archive::new(decompressor), dest, binary_name)
}

fn extract_from_tar_gz(
    archive_path: &Path,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive_path)?;
    let decompressor = flate2::read::GzDecoder::new(file);
    extract_named_from_tar(tar::Archive::new(decompressor), dest, binary_name)
}

fn extract_named_from_tar<R: std::io::Read>(
    mut archive: tar::Archive<R>,
    dest: &Path,
    binary_name: &str,
) -> Result<(), InstallError> {
    for entry in archive
        .entries()
        .map_err(|e| InstallError::Extract(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| InstallError::Extract(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| InstallError::Extract(e.to_string()))?;
        if path.file_name().and_then(|n| n.to_str()) == Some(binary_name) {
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(InstallError::Extract(format!(
        "{binary_name} not found in tar archive"
    )))
}

fn extract_zip_tree(archive_path: &Path, dest: &Path) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| InstallError::Extract(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| InstallError::Extract(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with('/') {
            continue;
        }
        let out_path = dest.join(name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn extract_tar_tree(archive_path: &Path, dest: &Path, xz: bool) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive_path)?;
    let reader: Box<dyn std::io::Read> = if xz {
        Box::new(xz2::read::XzDecoder::new(file))
    } else {
        Box::new(flate2::read::GzDecoder::new(file))
    };
    let mut archive = tar::Archive::new(reader);
    archive
        .unpack(dest)
        .map_err(|e| InstallError::Extract(e.to_string()))?;
    Ok(())
}

fn single_payload_dir(root: &Path) -> Result<PathBuf, InstallError> {
    let mut dirs = Vec::new();
    let mut files = 0usize;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        } else {
            files += 1;
        }
    }
    if dirs.len() == 1 && files == 0 {
        return Ok(dirs.pop().expect("one dir"));
    }
    Ok(root.to_path_buf())
}

fn copy_dir_merge(src: &Path, dest: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_merge(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub async fn download_with_mirrors(
    client: &Client,
    urls: &[String],
    temp_dir: &Path,
    quiet: bool,
) -> Result<(PathBuf, ArchiveFormat), InstallError> {
    use super::probe::pick_fastest_url;
    use tracing::{info, warn};

    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let start_idx = pick_fastest_url(client, &url_refs)
        .await
        .ok_or(InstallError::ProbeFailed)?;

    let mut ordered = Vec::with_capacity(urls.len());
    ordered.push(urls[start_idx].clone());
    for (i, url) in urls.iter().enumerate() {
        if i != start_idx {
            ordered.push(url.clone());
        }
    }

    let mut last_err = InstallError::Download("no mirror attempted".into());
    for url in ordered {
        let format = format_for_url(&url);
        let archive_path = temp_dir.join(archive_filename(&url));
        if !quiet {
            info!(url = %url, "downloading dependency");
        }
        match download_file(client, &url, &archive_path).await {
            Ok(()) => return Ok((archive_path, format)),
            Err(e) => {
                warn!(url = %url, error = %e, "download failed; trying next mirror");
                last_err = e;
                let _ = tokio::fs::remove_file(&archive_path).await;
            }
        }
    }
    Err(last_err)
}

pub fn format_for_url(url: &str) -> ArchiveFormat {
    if url.ends_with(".zip") {
        ArchiveFormat::Zip
    } else if url.ends_with(".7z") {
        ArchiveFormat::SevenZip
    } else if url.ends_with(".tar.xz") {
        ArchiveFormat::TarXz
    } else {
        ArchiveFormat::TarGz
    }
}
