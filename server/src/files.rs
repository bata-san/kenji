use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};

/// Workspace folder shared by API-key principals (`key:<name>`), whose
/// synthetic names are not valid Windows folder names.
pub const SHARED_USER_FOLDER: &str = "_shared";

/// File API scope. `Workspace` is the per-user sandbox. `System` is the whole
/// local PC and is restricted to administrators by the route layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsScope {
    Workspace,
    System,
}

impl FsScope {
    pub fn parse(raw: Option<&str>) -> Result<FsScope> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("workspace") => Ok(FsScope::Workspace),
            Some("system") => Ok(FsScope::System),
            Some(other) => Err(anyhow!(
                "scope は workspace または system を指定してください: {other}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FsScope::Workspace => "workspace",
            FsScope::System => "system",
        }
    }
}

/// True when the principal is an API key rather than a user database account.
pub fn is_api_key_principal(username: &str) -> bool {
    username.starts_with("key:")
}

/// Validate a username that will also become a folder name under the workspace.
pub fn validate_user_folder_name(username: &str) -> Result<()> {
    if is_api_key_principal(username) || username.trim() != username {
        return Err(anyhow!("invalid user folder name"));
    }
    let name = safe_file_name(username)?;
    if name.eq_ignore_ascii_case(SHARED_USER_FOLDER) {
        return Err(anyhow!("reserved user folder name"));
    }
    Ok(())
}

/// Folder name used for a principal. API keys share `_shared`; every database
/// user gets a folder named after the username.
pub fn user_folder_name(username: &str) -> Result<String> {
    if is_api_key_principal(username) {
        return Ok(SHARED_USER_FOLDER.to_string());
    }
    validate_user_folder_name(username)?;
    Ok(username.to_string())
}

/// Absolute (not yet guaranteed to exist) root folder for a principal.
pub fn user_root(workspace_root: &Path, username: &str) -> Result<PathBuf> {
    Ok(workspace_root.join(user_folder_name(username)?))
}

/// Create and return a principal's dedicated root. Fails closed when the path
/// is a reparse point or resolves somewhere other than the expected folder, so
/// one user cannot alias another user's root with a junction/symlink.
pub fn ensure_user_root(workspace_root: &Path, username: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(workspace_root)?;
    let root = user_root(workspace_root, username)?;
    std::fs::create_dir_all(&root)?;

    let canon_workspace = std::fs::canonicalize(workspace_root)?;
    let canon_root = std::fs::canonicalize(&root)?;
    let expected = canon_workspace.join(user_folder_name(username)?);
    if !same_path(&canon_root, &expected) {
        return Err(anyhow!("user workspace folder resolves outside its root"));
    }
    let meta = std::fs::symlink_metadata(&root)?;
    if !meta.is_dir() || is_reparse_point(&meta) {
        return Err(anyhow!("user workspace folder is not a real directory"));
    }
    // Return the caller-facing path (not the canonical `\\?\...` form) after
    // validating that it really resolves to the expected folder.
    Ok(root)
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

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

// ---------------------------------------------------------------------------
// System-wide filesystem scope (administrators only)
// ---------------------------------------------------------------------------

/// Normalize a system-scope path to the virtual form used by the API:
/// `/` for the drive list, `/C:` for a drive root, `/C:/Users/foo` below it.
/// Absolute native paths (`C:\Users\foo`, `C:/Users/foo`) are accepted too.
pub fn normalize_system_path(input: &str) -> Result<String> {
    // Only a wholly blank input is the virtual root. Do not trim components:
    // Windows silently strips trailing spaces/dots, so `C:\report ` aliases
    // `C:\report` and must be rejected by safe_file_name instead.
    if input.trim().is_empty() || input == "/" || input == "\\" {
        return Ok("/".to_string());
    }
    let replaced = input.replace('\\', "/");
    // Reject UNC and device namespaces. Local drives only: network shares can
    // still be mapped to a drive by an administrator.
    if replaced.starts_with("//") || replaced.starts_with("/?/") || replaced.starts_with("/./") {
        return Err(anyhow!(
            "UNC/device paths are not supported in system scope"
        ));
    }
    let body = replaced.trim_start_matches('/');
    let bytes = body.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(anyhow!(
            "system scope requires an absolute drive path like C:\\Users"
        ));
    }
    if bytes.len() > 2 && bytes[2] != b'/' {
        return Err(anyhow!("drive-relative paths are not allowed"));
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let rest = if body.len() > 3 { &body[3..] } else { "" };
    let mut parts = Vec::new();
    for part in rest.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(anyhow!("path traversal rejected"));
        }
        safe_file_name(part)?;
        parts.push(part);
    }
    if parts.is_empty() {
        Ok(format!("/{drive}:"))
    } else {
        Ok(format!("/{drive}:/{}", parts.join("/")))
    }
}

/// Convert a normalized system path (`/C:/Users/foo`) to a native path
/// (`C:\Users\foo`). The virtual root `/` is not a filesystem path.
pub fn system_path_to_native(normalized: &str) -> Result<PathBuf> {
    if normalized == "/" {
        return Err(anyhow!("the system root is virtual"));
    }
    let body = normalized.trim_start_matches('/');
    let bytes = body.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(anyhow!("invalid system path"));
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut path = PathBuf::from(format!("{drive}:\\"));
    if bytes.len() > 2 {
        if bytes[2] != b'/' {
            return Err(anyhow!("invalid system path"));
        }
        for part in body[3..].split('/').filter(|part| !part.is_empty()) {
            safe_file_name(part)?;
            path.push(part);
        }
    }
    Ok(path)
}

/// Resolve an existing system path (or the nearest existing ancestor for a
/// path that is about to be created) and return its canonical native form plus
/// the normalized API path.
pub fn resolve_system_path(input: &str) -> Result<(PathBuf, String)> {
    let normalized = normalize_system_path(input)?;
    if normalized == "/" {
        return Err(anyhow!("the system root is virtual"));
    }
    let native = system_path_to_native(&normalized)?;
    let resolved = resolve_existing_or_parent(&native)?;
    Ok((resolved, normalized))
}

fn resolve_existing_or_parent(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
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

/// Human-readable path with the Windows verbatim prefix removed.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    }
}

fn comparable_path(path: &Path) -> String {
    display_path(path)
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_uppercase()
}

fn split_drive(key: &str) -> Option<(&str, &str)> {
    let bytes = key.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_uppercase() && bytes[1] == b':' {
        Some((&key[..2], &key[2..]))
    } else {
        None
    }
}

/// Critical roots that must not be deleted, renamed, or moved as a whole.
/// Their contents remain reachable; this only prevents catastrophic mistakes.
pub fn is_protected_root(path: &Path) -> bool {
    let key = comparable_path(path);
    let Some((_drive, rest)) = split_drive(&key) else {
        return false;
    };
    if rest.is_empty() {
        return true; // drive root, e.g. C:\
    }
    const PROTECTED: &[&str] = &[
        r"\WINDOWS",
        r"\PROGRAM FILES",
        r"\PROGRAM FILES (X86)",
        r"\PROGRAMDATA",
        r"\USERS",
        r"\$RECYCLE.BIN",
        r"\SYSTEM VOLUME INFORMATION",
        r"\RECOVERY",
        r"\BOOT",
        r"\EFI",
        r"\PROGRAMDATA\KENJI",
    ];
    if PROTECTED.contains(&rest) {
        return true;
    }
    // A user profile root (C:\Users\<name>) is protected; its contents are not.
    if let Some(profile) = rest.strip_prefix(r"\USERS\") {
        if !profile.is_empty() && !profile.contains('\\') {
            return true;
        }
    }
    false
}

fn is_sensitive_data_file(data_dir: &Path, path: &Path) -> bool {
    let dir = comparable_path(data_dir);
    let key = comparable_path(path);
    let Some(rest) = key.strip_prefix(&format!("{dir}\\")) else {
        return false;
    };
    matches!(rest, "AUTH.JSON" | "CONFIG.JSON" | "JOBS.JSON")
        || rest.starts_with("USERS.DB")
        || rest.starts_with("AUTH.JSON")
}

/// Guard destructive system-scope operations against the OS and Kenji itself.
pub fn ensure_system_destructive_allowed(path: &Path, data_dir: &Path) -> Result<()> {
    if is_protected_root(path) {
        return Err(anyhow!(
            "このパスは重要なシステム領域のため、削除・移動できません"
        ));
    }
    if comparable_path(path) == comparable_path(data_dir) {
        return Err(anyhow!("Kenji のデータディレクトリは削除・移動できません"));
    }
    if is_sensitive_data_file(data_dir, path) {
        return Err(anyhow!("Kenji の機密ファイルは削除・移動できません"));
    }
    Ok(())
}

/// Guard writes (upload/mkdir/move destination) in system scope.
pub fn ensure_system_write_allowed(path: &Path, data_dir: &Path) -> Result<()> {
    if is_sensitive_data_file(data_dir, path) {
        return Err(anyhow!("Kenji の機密ファイルは上書きできません"));
    }
    Ok(())
}

/// Drive letters visible to the service account, e.g. `["C:", "D:"]`.
pub fn list_drives() -> Vec<String> {
    #[cfg(windows)]
    {
        (b'A'..=b'Z')
            .filter_map(|byte| {
                let letter = byte as char;
                if PathBuf::from(format!("{letter}:\\")).is_dir() {
                    Some(format!("{letter}:"))
                } else {
                    None
                }
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
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
    fn user_roots_are_created_from_usernames() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("workspace");

        let alice = ensure_user_root(&ws, "alice").unwrap();
        assert_eq!(alice, ws.join("alice"));
        assert!(alice.is_dir());

        let shared = ensure_user_root(&ws, "key:deploy").unwrap();
        assert_eq!(shared, ws.join(SHARED_USER_FOLDER));
        assert!(shared.is_dir());

        // A real user must not be able to claim the API-key shared folder.
        assert!(ensure_user_root(&ws, "_shared").is_err());
        assert!(ensure_user_root(&ws, "_SHARED").is_err());
        // Windows reserved names and unsafe folder names are rejected.
        assert!(ensure_user_root(&ws, "CON").is_err());
        assert!(ensure_user_root(&ws, "report.").is_err());
        assert!(ensure_user_root(&ws, "../escape").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn user_root_junction_to_another_user_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("workspace");
        let bob = ensure_user_root(&ws, "bob").unwrap();
        let alice = ws.join("alice");
        let output = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&alice)
            .arg(&bob)
            .output()
            .unwrap();
        assert!(output.status.success(), "mklink /J failed: {:?}", output);
        assert!(ensure_user_root(&ws, "alice").is_err());
        std::fs::remove_dir(&alice).unwrap();
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

    #[test]
    fn system_paths_are_normalized_and_absolute() {
        assert_eq!(normalize_system_path("/").unwrap(), "/");
        assert_eq!(normalize_system_path("").unwrap(), "/");
        assert_eq!(normalize_system_path("/C:").unwrap(), "/C:");
        assert_eq!(normalize_system_path("c:\\").unwrap(), "/C:");
        assert_eq!(
            normalize_system_path("/c:/Users/foo").unwrap(),
            "/C:/Users/foo"
        );
        assert_eq!(
            normalize_system_path(r"C:\Users\foo").unwrap(),
            "/C:/Users/foo"
        );
        assert_eq!(
            normalize_system_path("C:/Users//foo/./bar/").unwrap(),
            "/C:/Users/foo/bar"
        );
        #[cfg(windows)]
        assert_eq!(
            system_path_to_native("/C:/Users/foo").unwrap(),
            PathBuf::from(r"C:\Users\foo")
        );
    }

    #[test]
    fn system_paths_reject_escapes_and_devices() {
        for bad in [
            "../foo",
            "C:foo",
            "C:/foo/../bar",
            r"\\?\C:\Windows",
            r"\\.\C:\Windows",
            r"\\server\share",
            "C:/CON",
            "C:/foo:bar",
            "C:/report.",
            "C:/report ",
            "D:/ok", // drive may not exist, but normalization itself is valid
        ] {
            let normalized = normalize_system_path(bad);
            if bad == "D:/ok" {
                assert!(normalized.is_ok(), "{bad:?}");
            } else {
                assert!(normalized.is_err(), "{bad:?}");
            }
        }
    }

    #[test]
    fn protected_roots_cover_os_and_user_profiles() {
        for protected in [
            r"C:\",
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\ProgramData",
            r"C:\ProgramData\kenji",
            r"C:\Users",
            r"C:\Users\alice",
            r"D:\Windows",
        ] {
            assert!(is_protected_root(Path::new(protected)), "{protected:?}");
        }
        for allowed in [
            r"C:\Windows\System32\drivers\etc\hosts",
            r"C:\Users\alice\Desktop",
            r"C:\ProgramData\kenji\workspace\alice",
            r"C:\ProgramData\kenji\logs",
        ] {
            assert!(!is_protected_root(Path::new(allowed)), "{allowed:?}");
        }
    }

    #[test]
    fn sensitive_kenji_files_cannot_be_destroyed() {
        let data = Path::new(r"C:\ProgramData\kenji");
        for sensitive in [
            r"C:\ProgramData\kenji\auth.json",
            r"C:\ProgramData\kenji\auth.json.bak",
            r"C:\ProgramData\kenji\users.db",
            r"C:\ProgramData\kenji\users.db-wal",
            r"C:\ProgramData\kenji\users.db-shm",
            r"C:\ProgramData\kenji\users.db-journal",
            r"C:\ProgramData\kenji\users.db.backup",
            r"C:\ProgramData\kenji\config.json",
            r"C:\ProgramData\kenji\jobs.json",
        ] {
            assert!(
                ensure_system_destructive_allowed(Path::new(sensitive), data).is_err(),
                "{sensitive:?}"
            );
            assert!(
                ensure_system_write_allowed(Path::new(sensitive), data).is_err(),
                "{sensitive:?}"
            );
        }
        assert!(ensure_system_write_allowed(
            Path::new(r"C:\ProgramData\kenji\workspace\alice\a.txt"),
            data
        )
        .is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn drive_list_contains_system_drive() {
        assert!(list_drives().iter().any(|d| d == "C:"));
    }
}
