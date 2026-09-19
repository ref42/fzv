//! Where fzv keeps its own files.
//!
//! fzv writes nothing outside the versions directory: the download index cache
//! and the install locks both live in `<versions>/.fzv`, which also makes them
//! easy to find (and delete).

use crate::error::{Result, err};
use std::path::{Path, PathBuf};

/// Name of the state directory inside a versions directory.
pub const STATE_DIR: &str = ".fzv";

/// Whether `root` is one of fzv's versions directories.
///
/// Every install takes an install lock first, so the state directory exists as
/// soon as fzv has touched a versions directory — which makes this a reliable
/// marker even when a version directory is empty (an interrupted install).
pub fn is_versions_root(root: &Path) -> bool {
    root.join(STATE_DIR).is_dir()
}

/// The state directory for `root`, creating it on demand.
pub fn state_dir(root: &Path) -> Result<PathBuf> {
    let directory = root.join(STATE_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|error| err!("unable to create {}: {error}", directory.display()))?;
    Ok(directory)
}

/// Where the download index is cached for `root`.
pub fn index_cache(root: &Path) -> Result<PathBuf> {
    Ok(state_dir(root)?.join("download-index.json"))
}

/// The directory holding the `zig`/`zls` shims for `root`.
///
/// It is the only fzv entry in `PATH` once shims are installed, which is why it
/// lives under the versions directory: switching versions then never touches
/// `PATH` again.
pub fn shim_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join("bin")
}

/// The file recording the active version while shims are installed.
pub fn active_file(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join("active")
}

/// Marks that the "this terminal predates the `PATH` entry" hint was shown for
/// `root`, returning whether it is due.
///
/// A process cannot change its parent's environment, so the hint is worth
/// showing exactly once per versions directory; on every later command it would
/// only be noise.
pub fn session_hint_once(root: &Path) -> bool {
    let marker = root.join(STATE_DIR).join("session-hint");
    if marker.is_file() {
        return false;
    }
    // If the marker cannot be written the hint may repeat, which is a far
    // smaller problem than failing an otherwise successful command.
    let _ = std::fs::write(&marker, b"shown\n");
    true
}

/// The install lock file for one target (a version, or `zls`).
pub fn lock_file(root: &Path, name: &str) -> Result<PathBuf> {
    let directory = state_dir(root)?.join("locks");
    std::fs::create_dir_all(&directory)
        .map_err(|error| err!("unable to create {}: {error}", directory.display()))?;
    Ok(directory.join(format!("{}.lock", sanitize_name(name))))
}

/// Names are used as file names, so anything unexpected is replaced.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || ".+_-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_under_the_versions_directory() {
        let root = std::env::temp_dir().join(format!("fzv-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cache = index_cache(&root).unwrap();
        assert!(cache.starts_with(&root));
        assert!(cache.parent().unwrap().is_dir());
        let lock = lock_file(&root, "0.14.1").unwrap();
        assert_eq!(lock.file_name().unwrap(), "0.14.1.lock");
        assert_eq!(
            lock_file(&root, "0.14.1/../x")
                .unwrap()
                .file_name()
                .unwrap(),
            "0.14.1_.._x.lock"
        );
        // The shim mode files stay inside the state directory too.
        assert!(shim_dir(&root).ends_with(Path::new(STATE_DIR).join("bin")));
        assert!(active_file(&root).ends_with(Path::new(STATE_DIR).join("active")));
        assert!(shim_dir(&root).starts_with(&root));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_session_hint_is_due_only_once() {
        let root = std::env::temp_dir().join(format!("fzv-hint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        state_dir(&root).unwrap();
        assert!(session_hint_once(&root));
        assert!(!session_hint_once(&root));
        // A second versions directory has its own answer.
        let other = root.join("other");
        state_dir(&other).unwrap();
        assert!(session_hint_once(&other));
        std::fs::remove_dir_all(root).unwrap();
    }
}
