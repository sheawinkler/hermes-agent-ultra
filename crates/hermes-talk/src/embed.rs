use std::path::PathBuf;

/// No embedded model files — all models are bundled in the package directory.
pub fn ensure_extracted() -> Option<PathBuf> {
    None
}
