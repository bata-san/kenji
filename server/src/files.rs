use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};

/// Resolve a user-supplied workspace-relative path (e.g. "src", "/src/main.rs")
/// to an absolute path, rejecting traversal and absolute/prefixed paths.
pub fn resolve_under_root(root: &Path, user_path: &str) -> Result<PathBuf> {
    let trimmed = user_path.trim().trim_start_matches('/');
    let trimmed = trimmed.replace('\\', "/");
    if trimmed.is_empty() {
        return Ok(root.to_path_buf());
    }
    let rel = Path::new(&trimmed);
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir => return Err(anyhow!("path traversal rejected")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("absolute paths are not allowed"))
            }
        }
    }
    let mut p = root.to_path_buf();
    p.push(rel);
    Ok(p)
}

/// Defense-in-depth check that an existing path stays inside the root.
pub fn ensure_within(root: &Path, path: &Path) -> Result<PathBuf> {
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon = std::fs::canonicalize(path)?;
    if canon.starts_with(&canon_root) {
        Ok(canon)
    } else {
        Err(anyhow!("path escapes workspace"))
    }
}

/// A safe single path component for a client-supplied file name.
pub fn safe_file_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name == "."
        || name == ".."
    {
        return Err(anyhow!("invalid file name"));
    }
    // Reject reserved Windows characters
    if name.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | '\0')) {
        return Err(anyhow!("invalid file name"));
    }
    Ok(name.to_string())
}

/// Human-readable bytes (kept for API parity / CLI use later).
#[allow(dead_code)]
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        let root = Path::new("C:/data/ws");
        assert!(resolve_under_root(root, "../x").is_err());
        assert!(resolve_under_root(root, "a/../../b").is_err());
        assert!(resolve_under_root(root, "C:/Windows").is_err());
        assert!(resolve_under_root(root, "\\\\server\\share").is_err());
        assert!(resolve_under_root(root, "src/main.rs").is_ok());
        assert!(resolve_under_root(root, "/").is_ok());
    }

    #[test]
    fn file_name_rules() {
        assert!(safe_file_name("a b.txt").is_ok());
        assert!(safe_file_name("..").is_err());
        assert!(safe_file_name("a/b").is_err());
        assert!(safe_file_name("a:b").is_err());
    }
}
