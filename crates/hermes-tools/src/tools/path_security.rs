//! Shared path validation helpers for tool implementations.
//!
//! Extracts the `canonicalize() + strip_prefix()` and `..` traversal check
//! patterns previously duplicated across various tools.

use std::path::{Component, Path, PathBuf};

fn normalize_existing_path(path: &Path) -> std::io::Result<PathBuf> {
    if let Ok(resolved) = path.canonicalize() {
        return Ok(resolved);
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => suffix.push(name.to_os_string()),
            None => break,
        }
        existing = existing.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no existing path component")
        })?;
    }

    let mut resolved = existing.canonicalize()?;
    for component in suffix.iter().rev() {
        match Path::new(component).components().next() {
            Some(Component::ParentDir) => {
                resolved.pop();
            }
            Some(Component::CurDir) => {}
            _ => resolved.push(component),
        }
    }

    Ok(resolved)
}

/// Ensure `path` resolves to a location within `root`.
///
/// Returns an error message string if validation fails, or `None` if the
/// path is safe. Uses `canonicalize()` to follow symlinks and normalize
/// `..` components.
///
/// # Example
///
/// ```
/// use std::path::PathBuf;
/// use hermes_tools::tools::path_security::validate_within_dir;
///
/// let root = PathBuf::from("/tmp/safe");
/// let user_path = PathBuf::from("/tmp/safe/subdir/file.txt");
///
/// if let Some(error) = validate_within_dir(&user_path, &root) {
///     eprintln!("Validation failed: {}", error);
/// }
/// ```
pub fn validate_within_dir(path: &Path, root: &Path) -> Option<String> {
    // Canonicalize both paths to resolve symlinks and normalize .. components
    let resolved = match normalize_existing_path(path) {
        Ok(p) => p,
        Err(e) => return Some(format!("Path escapes allowed directory: {}", e)),
    };

    let root_resolved = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => return Some(format!("Path escapes allowed directory: {}", e)),
    };

    // Check if resolved path starts with root
    match resolved.strip_prefix(&root_resolved) {
        Ok(_) => None,
        Err(e) => Some(format!("Path escapes allowed directory: {}", e)),
    }
}

/// Return true if `path_str` contains `..` traversal components.
///
/// Quick check for obvious traversal attempts before doing full resolution.
///
/// # Example
///
/// ```
/// use hermes_tools::tools::path_security::has_traversal_component;
///
/// assert!(has_traversal_component("foo/../bar"));
/// assert!(has_traversal_component("../etc/passwd"));
/// assert!(!has_traversal_component("foo/bar"));
/// ```
pub fn has_traversal_component(path_str: &str) -> bool {
    Path::new(path_str)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_has_traversal_component_with_dotdot() {
        assert!(has_traversal_component("foo/../bar"));
        assert!(has_traversal_component("../etc/passwd"));
        assert!(has_traversal_component("../../secret"));
        assert!(has_traversal_component("foo/../../bar"));
    }

    #[test]
    fn test_has_traversal_component_without_dotdot() {
        assert!(!has_traversal_component("foo/bar"));
        assert!(!has_traversal_component("foo/bar/baz"));
        assert!(!has_traversal_component("normal_file.txt"));
        assert!(!has_traversal_component(""));
    }

    #[test]
    fn test_has_traversal_component_edge_cases() {
        // Single or double dots as part of filename shouldn't trigger
        assert!(!has_traversal_component("foo..bar"));
        assert!(!has_traversal_component("...weird"));
        // But actual parent directory component should
        assert!(has_traversal_component("foo/.."));
    }

    #[test]
    fn test_validate_within_dir_safe_path() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("path_security_test_root");
        let subdir = test_root.join("subdir");

        // Setup
        let _ = fs::create_dir_all(&subdir);

        // Test path within root
        let result = validate_within_dir(&subdir, &test_root);
        assert!(result.is_none(), "Safe path should pass validation");

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }

    #[test]
    fn test_validate_within_dir_same_path() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("path_security_test_same");

        // Setup
        let _ = fs::create_dir_all(&test_root);

        // Test root path itself
        let result = validate_within_dir(&test_root, &test_root);
        assert!(result.is_none(), "Root path itself should be valid");

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }

    #[test]
    fn test_validate_within_dir_escape_attempt() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("path_security_test_escape");
        let outside_path = temp_dir.join("outside");

        // Setup
        let _ = fs::create_dir_all(&test_root);
        let _ = fs::create_dir_all(&outside_path);

        // Test path outside root
        let result = validate_within_dir(&outside_path, &test_root);
        assert!(result.is_some(), "Path outside root should fail validation");
        assert!(result.unwrap().contains("escapes allowed directory"));

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
        let _ = fs::remove_dir_all(&outside_path);
    }

    #[test]
    fn test_validate_within_dir_nonexistent_path() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("path_security_test_nonexist");
        let nonexist = test_root.join("does_not_exist");

        // Setup
        let _ = fs::create_dir_all(&test_root);

        // Python Path.resolve() allows nonexistent leaf paths.
        let result = validate_within_dir(&nonexist, &test_root);
        assert!(
            result.is_none(),
            "Nonexistent path inside root should pass validation"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }

    #[test]
    fn test_validate_within_dir_with_traversal() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("path_security_test_traversal");
        let subdir = test_root.join("subdir");

        // Setup
        let _ = fs::create_dir_all(&subdir);

        // Create a path with .. that still resolves within root
        // This tests that canonicalize properly handles ..
        let safe_traversal = subdir.join("../subdir");
        let result = validate_within_dir(&safe_traversal, &test_root);
        assert!(result.is_none(), "Safe traversal should pass");

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }
}
