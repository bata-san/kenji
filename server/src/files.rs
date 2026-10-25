use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};

/// Resolve a user-supplied workspace-relative path (e.g. "src", "/src/main.rs")
/// to an absolute path, rejecting traversal and absolute/prefixed paths.
pub fn resolve_under_root(root: &Path, user_path: &str) -> Result<PathBuf> {
    let trimmed = user_path.trim_start_matches('/');
    let trimmed = trimmed.replace('\\', "/");
    if trimmed.is_empty() {
        return Ok(root.to_path_buf());
    }
    // Validate every raw component before Path normalizes away `.` or aliases.
    for component in trimmed.split('/').filter(|part| !part.is_empty()) {
        safe_file_name(component)?;
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
/// This is a path-based check, not protection against concurrent parent/junction
/// swaps. Fully closing that race requires handle-relative filesystem operations.
pub fn ensure_within(root: &Path, path: &Path) -> Result<PathBuf> {
    let canon_root = std::fs::canonicalize(root)?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // For operations creating a path, resolve the nearest existing ancestor. This
    // catches symlinked parents while allowing the leaf itself not to exist yet.
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(canon_ancestor) => {
                if !canon_ancestor.starts_with(&canon_root) {
                    return Err(anyhow!("path escapes workspace"));
                }
                let mut resolved = canon_ancestor;
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| anyhow!("path has no existing ancestor"))?;
                suffix.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| anyhow!("path has no existing ancestor"))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Reject destructive operations on the canonical workspace root, including aliases.
pub fn ensure_not_root(root: &Path, path: &Path) -> Result<()> {
    if ensure_within(root, path)? == std::fs::canonicalize(root)? {
        return Err(anyhow!("workspace root cannot be moved or deleted"));
    }
    Ok(())
}

/// A safe single path component for a client-supplied file name.
pub fn safe_file_name(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
    {
        return Err(anyhow!("invalid file name"));
    }
    // Reject reserved Windows characters and device names (including extensions).
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(anyhow!("invalid file name"));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    if reserved {
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
        assert!(safe_file_name("report.").is_err());
        assert!(safe_file_name("CON.txt").is_err());
        assert!(safe_file_name("nul").is_err());
        assert!(safe_file_name("COM1.log").is_err());
        assert!(safe_file_name("LPT9").is_err());
        assert!(safe_file_name("normal.txt").is_ok());
    }

    #[test]
    fn rejects_windows_aliases_in_all_components() {
        for name in [
            "report ",
            "report. ",
            "CON .txt",
            "COM¹.txt",
            "LPT²",
            "CONIN$",
            "CONOUT$.log",
            "a\u{7f}",
            "a\n",
        ] {
            assert!(safe_file_name(name).is_err(), "{name:?}");
            assert!(
                resolve_under_root(Path::new("workspace"), &format!("{name}/child")).is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn root_guards_and_missing_root_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let alias = root.join(".");
        assert!(ensure_not_root(&root, &alias).is_err());
        assert!(ensure_not_root(&root, &std::fs::canonicalize(&root).unwrap()).is_err());
        assert!(ensure_not_root(&root, &root.join("child")).is_ok());
        std::fs::remove_dir(&root).unwrap();
        assert!(ensure_within(&root, &root.join("child")).is_err());
        assert!(!root.exists());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_junction_escapes_and_root_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        for (name, target) in [("escape", &outside), ("root-alias", &root)] {
            let link = root.join(name);
            let output = std::process::Command::new("cmd")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&link)
                .arg(target)
                .output()
                .unwrap();
            assert!(output.status.success(), "mklink /J failed: {:?}", output);
            if name == "escape" {
                assert!(ensure_within(&root, &link.join("new/child")).is_err());
                assert!(ensure_within(&root, &link).is_err());
            } else {
                assert!(ensure_not_root(&root, &link).is_err());
            }
            std::fs::remove_dir(link).unwrap();
        }
    }

    #[test]
    fn ensure_within_resolves_existing_and_missing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let existing = root.join("existing.txt");
        std::fs::write(&existing, "data").unwrap();
        assert_eq!(
            ensure_within(&root, &existing).unwrap(),
            std::fs::canonicalize(&existing).unwrap()
        );
        let resolved = ensure_within(&root, &root.join("new/subdir")).unwrap();
        assert!(resolved.ends_with("new/subdir"));
        assert!(resolved.starts_with(std::fs::canonicalize(&root).unwrap()));
        assert!(ensure_within(&root, &temp.path().join("outside.txt")).is_err());
    }
}
