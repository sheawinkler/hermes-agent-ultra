//! ripgrep binary bytes embedded at **build** time for the compiling TARGET triple.

include!(concat!(env!("OUT_DIR"), "/bundled_rg.rs"));

use std::path::Path;

/// Pinned ripgrep release version (from `rg-version.txt` at build time).
pub fn version() -> &'static str {
    env!("HERMES_BUNDLED_RG_VERSION")
}

/// Write embedded `rg` to `dest` when missing.
pub fn materialize(dest: &Path) -> std::io::Result<()> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, RG_BYTES)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dest, perms)?;
    }
    Ok(())
}
