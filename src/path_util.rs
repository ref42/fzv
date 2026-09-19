//! Pure path and `PATH` handling.
//!
//! fzv runs on Windows, so the rules here are the Windows ones: `;`-separated
//! `PATH` values, either separator inside a path, case-insensitive comparison,
//! and protection against touching unrelated entries. They are kept free of any
//! registry or environment access — the platform layer passes the raw value in —
//! which also makes them directly testable.

use std::path::{Path, PathBuf};

/// The conventions of the platform whose `PATH` is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathStyle {
    /// Separates entries in a `PATH` value: `';'` on Windows, `':'` elsewhere.
    pub separator: char,
    /// Separates directories inside a path: `'\\'` on Windows, `'/'` elsewhere.
    pub native_separator: char,
    /// Windows treats both separators as equivalent and ignores case.
    pub windows: bool,
}

impl PathStyle {
    /// The conventions fzv runs with: Windows `PATH` values.
    pub fn windows() -> Self {
        PathStyle {
            separator: ';',
            native_separator: '\\',
            windows: true,
        }
    }

    /// The Unix conventions, kept so that the rules below stay exercised (and
    /// documented) by tests even though only the Windows backend ships.
    pub fn unix() -> Self {
        PathStyle {
            separator: ':',
            native_separator: '/',
            windows: false,
        }
    }
}

/// `fs::canonicalize`, minus the extended-length (`\\?\`) prefix Windows adds:
/// that form does not belong in a configuration value or in the user's `PATH`.
pub fn canonical_path(path: &Path, style: PathStyle) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(PathBuf::from(strip_verbatim(
        &canonical.to_string_lossy(),
        style,
    )))
}

/// Removes the verbatim prefix Windows uses for extended-length paths.
pub fn strip_verbatim(path: &str, style: PathStyle) -> String {
    if !style.windows {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// Removes one pair of surrounding double quotes (never legal inside a Windows
/// file name, so it can only be shell noise).
pub fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(text)
}

/// Normalizes a `PATH` entry so that two spellings of the same directory compare
/// equal: verbatim prefix dropped, quotes removed, separators unified, trailing
/// separators dropped and, on Windows, case folded.
pub fn path_key(path: &str, style: PathStyle) -> String {
    let text = strip_verbatim(path.trim(), style);
    let text = unquote(&text).to_string();
    let mut text = if style.windows {
        text.replace('/', "\\").to_ascii_lowercase()
    } else {
        text
    };
    while text.len() > 1 && text.ends_with(['\\', '/']) {
        text.pop();
    }
    text
}

/// The prefix that marks `PATH` entries below a versions directory, or `None`
/// when the directory is too close to the filesystem root to be matched safely
/// (a bare drive, a UNC share, `/`).
pub fn path_scope(key: &str, style: PathStyle) -> Option<String> {
    let trimmed = key.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return None;
    }
    // A bare drive such as "c:" covers a whole volume.
    if style.windows && trimmed.len() == 2 && trimmed.ends_with(':') {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("\\\\") {
        // A UNC share root ("\\server\share") covers a whole share.
        if rest.split('\\').filter(|part| !part.is_empty()).count() < 3 {
            return None;
        }
    } else if !trimmed.contains(['\\', '/']) {
        return None;
    }
    Some(format!("{trimmed}{}", style.native_separator))
}

/// True for the directory names fzv uses inside a versions directory.
pub fn is_version_dir_name(key: &str) -> bool {
    let name = key.rsplit(['\\', '/']).next().unwrap_or_default();
    name == "dev" || crate::version::Version::parse(name).is_some()
}

/// Whether `entry` has the shape of one of fzv's Zig directories, without
/// touching the filesystem.
pub fn looks_like_fzv_zig_dir(entry: &str, style: PathStyle) -> bool {
    let text = strip_verbatim(entry.trim(), style);
    is_absolute(&text, style) && is_version_dir_name(&path_key(&text, style))
}

/// True when a `PATH` entry is one of fzv's Zig directories: an absolute
/// directory named after a version that actually holds a Zig executable.
///
/// The executable check is what makes this safe. Unrelated tools also live in
/// version-named directories (`...\ninja\1.13.2`, `...\probe-rs\0.32.0`), and
/// those must never be mistaken for a Zig version or removed from `PATH`.
pub fn is_fzv_zig_dir(entry: &str, style: PathStyle, zig_executable: &str) -> bool {
    if !looks_like_fzv_zig_dir(entry, style) {
        return false;
    }
    let text = strip_verbatim(entry.trim(), style);
    Path::new(&text).join(zig_executable).is_file()
}

/// The first entry of a `PATH` value that is one of fzv's Zig directories.
pub fn zig_dir_in_path(value: &str, style: PathStyle, zig_executable: &str) -> Option<PathBuf> {
    value
        .split(style.separator)
        .map(str::trim)
        .find(|entry| is_fzv_zig_dir(entry, style, zig_executable))
        .map(|entry| PathBuf::from(strip_verbatim(entry, style)))
}

/// The form a path takes inside a `PATH` value: native separators and no
/// verbatim prefix, so `D:/zig` and `D:\zig` produce the same entry.
pub fn display_path(path: &Path, style: PathStyle) -> String {
    let text = strip_verbatim(&path.to_string_lossy(), style);
    if style.windows {
        text.replace('/', "\\")
    } else {
        text
    }
}

/// Whether `text` is absolute according to `style` rather than to the host.
pub fn is_absolute(text: &str, style: PathStyle) -> bool {
    if style.windows {
        let bytes = text.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
            || text.starts_with(r"\\")
    } else {
        text.starts_with('/')
    }
}

/// True for `D:name`: a drive letter followed by a name without separators,
/// which is what is left of `D:\name` once a shell eats the backslashes.
pub fn is_drive_relative(text: &str, style: PathStyle) -> bool {
    if !style.windows {
        return false;
    }
    let bytes = text.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && !text[2..].contains(['\\', '/'])
}

/// Rewrites a `PATH` value for `root`, inserting `wanted` first when given.
///
/// Two kinds of entries belong to fzv and are dropped:
/// * anything below `root` (when `root` is deep enough for a safe prefix match),
/// * entries recognised as one of fzv's Zig directories, which is how a previous
///   versions directory is found again after switching to a new one.
///
/// Every other entry is preserved verbatim.
pub fn rewrite_path(
    old: &str,
    root: &Path,
    wanted: Option<&Path>,
    style: PathStyle,
    zig_executable: &str,
) -> String {
    let scope = path_scope(&path_key(&root.to_string_lossy(), style), style);
    let wanted_key = wanted.map(|path| path_key(&path.to_string_lossy(), style));
    let mut entries = Vec::new();
    for entry in old.split(style.separator).map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        let key = path_key(entry, style);
        let under_root = scope
            .as_ref()
            .is_some_and(|scope| key.starts_with(scope.as_str()));
        if Some(&key) == wanted_key.as_ref()
            || under_root
            || is_fzv_zig_dir(entry, style, zig_executable)
        {
            continue;
        }
        entries.push(entry.to_string());
    }
    if let Some(wanted) = wanted {
        entries.insert(0, display_path(wanted, style));
    }
    entries.join(&style.separator.to_string())
}

/// The entries of `old` that [`rewrite_path`] would drop, so a caller can report
/// them instead of removing them silently.
pub fn dropped_entries(
    old: &str,
    new: &str,
    wanted: Option<&Path>,
    style: PathStyle,
) -> Vec<String> {
    let new_keys: Vec<String> = new.split(style.separator).map(|entry| path_key(entry, style)).collect();
    let wanted_key = wanted.map(|path| path_key(&path.to_string_lossy(), style));
    old.split(style.separator)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter(|entry| {
            let key = path_key(entry, style);
            Some(&key) != wanted_key.as_ref() && !new_keys.contains(&key)
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows() -> PathStyle {
        PathStyle::windows()
    }

    fn unix() -> PathStyle {
        PathStyle::unix()
    }

    #[test]
    fn normalizes_windows_entries() {
        let style = windows();
        let expected = path_key(r"C:\Zig\0.14.1", style);
        assert_eq!(expected, r"c:\zig\0.14.1");
        assert_eq!(path_key(r"C:\Zig\0.14.1\", style), expected);
        assert_eq!(path_key(r#"  "C:\Zig\0.14.1"  "#, style), expected);
        assert_eq!(path_key(r"\\?\C:\Zig\0.14.1", style), expected);
        assert_eq!(path_key("C:/Zig/0.14.1", style), expected);
    }

    #[test]
    fn normalizes_unix_entries() {
        let style = unix();
        let expected = path_key("/home/u/zig/0.14.1", style);
        assert_eq!(expected, "/home/u/zig/0.14.1");
        assert_eq!(path_key("/home/u/zig/0.14.1/", style), expected);
        assert_eq!(path_key(r#""/home/u/zig/0.14.1""#, style), expected);
        // Case and separators are significant, and a verbatim prefix is just a
        // file name on Unix.
        assert_ne!(path_key("/Home/u/zig/0.14.1", style), expected);
        assert_eq!(path_key(r"\\?\D:\zig", style), r"\\?\D:\zig");
    }

    #[test]
    fn scopes_reject_whole_volumes() {
        let style = windows();
        assert_eq!(path_scope(&path_key(r"D:\", style), style), None);
        assert_eq!(path_scope(&path_key("D:", style), style), None);
        assert_eq!(path_scope(&path_key(r"\\server\share", style), style), None);
        assert_eq!(path_scope("relative", style), None);
        assert_eq!(path_scope("", style), None);
        let deep = path_key(r"D:\zig", style);
        assert_eq!(path_scope(&deep, style), Some(r"d:\zig\".to_string()));

        let style = unix();
        assert_eq!(path_scope(&path_key("/", style), style), None);
        assert_eq!(path_scope("relative", style), None);
        let deep = path_key("/home/u/zig", style);
        assert_eq!(path_scope(&deep, style), Some("/home/u/zig/".to_string()));
    }

    #[test]
    fn recognises_absolute_paths_per_style() {
        assert!(is_absolute(r"D:\zig", windows()));
        assert!(is_absolute("D:/zig", windows()));
        assert!(is_absolute(r"\\server\share", windows()));
        assert!(!is_absolute("D:zig", windows()));
        assert!(!is_absolute(r"\zig", windows()));
        assert!(is_absolute("/home/u", unix()));
        assert!(!is_absolute("home/u", unix()));
        assert!(!is_absolute(r"D:\zig", unix()));
    }

    #[test]
    fn recognises_version_directory_names() {
        assert!(is_version_dir_name(&path_key(r"D:\zig\0.14.1", windows())));
        assert!(is_version_dir_name(&path_key(
            r"D:\zig\0.17.0-dev.2228+955228b68",
            windows()
        )));
        assert!(is_version_dir_name(&path_key(r"D:\zig\dev", windows())));
        assert!(!is_version_dir_name(&path_key(r"D:\zig", windows())));
        assert!(!is_version_dir_name(&path_key(r"D:\zig\zls", windows())));
        assert!(!is_version_dir_name(&path_key(r"D:\zig\0.14", windows())));
        assert!(!is_version_dir_name(&path_key(r"D:\zig\0.14.1\bin", windows())));
        assert!(is_version_dir_name(&path_key("/home/u/zig/0.14.1", unix())));
    }

    #[test]
    fn detects_drive_relative_input() {
        assert!(is_drive_relative("D:PL_Collectionszig", windows()));
        assert!(is_drive_relative("c:zig", windows()));
        assert!(!is_drive_relative(r"D:\zig", windows()));
        assert!(!is_drive_relative("D:/zig", windows()));
        assert!(!is_drive_relative("D:", windows()));
        assert!(!is_drive_relative("zig", windows()));
        assert!(!is_drive_relative("D:zig", unix()));
    }

    #[test]
    fn drops_only_managed_entries_windows() {
        let style = windows();
        let base = temp_dir("rewrite-windows");
        let old_root = base.join("zig");
        let new_root = base.join("zig2");
        for (root, version) in [
            (&old_root, "0.13.0"),
            (&old_root, "0.14.0"),
            (&new_root, "0.15.0"),
        ] {
            let directory = root.join(version);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("zig.exe"), b"exe").unwrap();
        }
        // An unrelated tool that also lives in a version-named directory.
        let ninja = base.join("ninja").join("1.13.2");
        std::fs::create_dir_all(&ninja).unwrap();
        std::fs::write(ninja.join("ninja.exe"), b"exe").unwrap();

        let verbatim = format!(r"\\?\{}", old_root.join("0.14.0").display());
        let old = format!(
            r"C:\Windows;{};{};{};{}",
            old_root.join("0.13.0").display(),
            verbatim,
            ninja.display(),
            base.join("other").display()
        );
        let wanted = new_root.join("0.15.0");
        let new = rewrite_path(&old, &new_root, Some(&wanted), style, "zig.exe");
        assert!(new.starts_with(&format!("{}{}", wanted.display(), style.separator)));
        for kept in [
            r"C:\Windows".to_string(),
            ninja.display().to_string(),
            base.join("other").display().to_string(),
        ] {
            assert!(new.contains(&kept), "{kept} was dropped from {new}");
        }
        assert!(!new.contains("0.13.0"), "{new} kept a managed entry");
        assert!(!new.contains("0.14.0"), "{new} kept a managed entry");

        // Removing the active version clears only the managed entry.
        let cleared = rewrite_path(&new, &new_root, None, style, "zig.exe");
        assert!(!cleared.contains("0.15.0"), "{cleared}");
        assert!(cleared.contains("ninja"), "{cleared}");

        // The dropped entries can be reported.
        let dropped = dropped_entries(&old, &new, Some(&wanted), style);
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert!(dropped.iter().all(|entry| entry.contains("0.1")));

        // A whole volume cannot be prefix-matched, so only recognised entries go.
        let volume = rewrite_path(&old, Path::new(r"D:\"), Some(&wanted), style, "zig.exe");
        assert!(volume.contains("ninja"), "{volume} dropped an unrelated entry");
        assert!(volume.contains(&base.join("other").display().to_string()), "{volume}");

        // Selecting the same version twice does not duplicate the entry.
        let twice = rewrite_path(&new, &new_root, Some(&wanted), style, "zig.exe");
        assert_eq!(twice.matches("0.15.0").count(), 1);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn keeps_unrelated_entries_unix_style() {
        // The filesystem checks cannot be satisfied for Unix paths on a Windows
        // host, so nothing is recognised as fzv's: the safe outcome.
        let style = unix();
        let old = "/usr/bin:/home/u/zig/0.14.1:/opt/ninja/1.13.2";
        let new = rewrite_path(old, Path::new("/home/u/zig"), Some(Path::new("/home/u/zig/0.15.0")), style, "zig");
        assert_eq!(
            new,
            "/home/u/zig/0.15.0:/usr/bin:/opt/ninja/1.13.2"
        );
        // Entries under the root are dropped by the scope rule.
        let old = "/usr/bin:/home/u/zig/0.14.1:/opt/tools";
        let new = rewrite_path(old, Path::new("/home/u/zig"), None, style, "zig");
        assert_eq!(new, "/usr/bin:/opt/tools");
    }

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-path-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
