use std::path::{Path, PathBuf};

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let target = std::env::var("TARGET").expect("TARGET");
    let version = std::fs::read_to_string(Path::new("rg-version.txt"))
        .expect("rg-version.txt")
        .trim()
        .trim_start_matches('v')
        .to_string();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=rg-version.txt");
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rustc-env=HERMES_BUNDLED_RG_VERSION={version}");

    let (suffix, ext) = target_triple(&target).expect("unsupported TARGET for bundled ripgrep");
    let archive = format!("ripgrep-{version}-{suffix}.{ext}");
    let url =
        format!("https://github.com/BurntSushi/ripgrep/releases/download/{version}/{archive}");

    let cache_dir = out_dir.join("vendor-cache");
    std::fs::create_dir_all(&cache_dir).expect("create cache dir");
    let archive_path = cache_dir.join(&archive);
    if !archive_path.is_file() {
        eprintln!("hermes-bundled-rg: downloading {url}");
        download(&url, &archive_path);
    }

    let binary_name = if target.contains("windows") {
        "rg.exe"
    } else {
        "rg"
    };
    let rg_out = out_dir.join(binary_name);
    extract_rg(&archive_path, ext, &rg_out, binary_name);

    let out_rs = out_dir.join("bundled_rg.rs");
    let bytes_path = rg_out.display().to_string().replace('\\', "/");
    std::fs::write(
        &out_rs,
        format!(
            "pub static RG_BYTES: &[u8] = include_bytes!(r\"{bytes_path}\");\n\
             pub static RG_FILENAME: &str = \"{binary_name}\";\n"
        ),
    )
    .expect("write bundled_rg.rs");
}

fn target_triple(target: &str) -> Option<(&'static str, &'static str)> {
    match target {
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => {
            Some(("x86_64-pc-windows-msvc", "zip"))
        }
        "aarch64-pc-windows-msvc" => Some(("aarch64-pc-windows-msvc", "zip")),
        "x86_64-unknown-linux-gnu" => Some(("x86_64-unknown-linux-gnu", "tar.gz")),
        "aarch64-unknown-linux-gnu" => Some(("aarch64-unknown-linux-gnu", "tar.gz")),
        "armv7-unknown-linux-gnueabihf" => Some(("arm-unknown-linux-gnueabihf", "tar.gz")),
        "x86_64-apple-darwin" => Some(("x86_64-apple-darwin", "tar.gz")),
        "aarch64-apple-darwin" => Some(("aarch64-apple-darwin", "tar.gz")),
        _ => None,
    }
}

fn download(url: &str, dest: &Path) {
    let client = reqwest::blocking::Client::builder()
        .user_agent("hermes-bundled-rg/build")
        .build()
        .expect("http client");
    let mut resp = client
        .get(url)
        .send()
        .expect("download rg")
        .error_for_status()
        .expect("http ok");
    let mut file = std::fs::File::create(dest).expect("create archive");
    resp.copy_to(&mut file).expect("write archive");
}

fn extract_rg(archive: &Path, ext: &str, dest: &Path, binary_name: &str) {
    if dest.is_file() {
        return;
    }
    let extract_root = archive.parent().expect("parent").join("rg-extract");
    if extract_root.exists() {
        let _ = std::fs::remove_dir_all(&extract_root);
    }
    std::fs::create_dir_all(&extract_root).expect("mkdir extract");

    match ext {
        "zip" => {
            let file = std::fs::File::open(archive).expect("open zip");
            let mut zip = zip::ZipArchive::new(file).expect("zip");
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).expect("zip entry");
                let name = entry.name().replace('\\', "/");
                if name.ends_with('/') {
                    continue;
                }
                let out = extract_root.join(&name);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let mut out_file = std::fs::File::create(&out).expect("zip out");
                std::io::copy(&mut entry, &mut out_file).expect("zip copy");
            }
        }
        "tar.gz" => {
            let file = std::fs::File::open(archive).expect("open tar.gz");
            let dec = flate2::read::GzDecoder::new(file);
            let mut tar = tar::Archive::new(dec);
            tar.unpack(&extract_root).expect("untar");
        }
        other => panic!("unsupported archive extension: {other}"),
    }

    let found = find_file(&extract_root, binary_name, 6).expect("rg binary in archive");
    std::fs::copy(&found, dest).expect("copy rg");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dest, perms).expect("chmod");
    }
}

fn find_file(root: &Path, name: &str, max_depth: u32) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u32, max: u32) -> Option<PathBuf> {
        if depth > max {
            return None;
        }
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = walk(&path, name, depth + 1, max)
            {
                return Some(found);
            }
        }
        None
    }
    walk(root, name, 0, max_depth)
}
