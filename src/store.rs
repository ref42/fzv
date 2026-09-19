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
    std::fs::create_dir_all(&directory).map_err(|error| {
        err!("unable to create {}: {error}", directory.display())
    })?;
    Ok(directory)
}

/// Where the download index is cached for `root`.
pub fn index_cache(root: &Path) -> Result<PathBuf> {
    Ok(state_dir(root)?.join("download-index.json"))
}

/// The install lock file for one target (a version, or `zls`).
pub fn lock_file(root: &Path, name: &str) -> Result<PathBuf> {
    let directory = state_dir(root)?.join("locks");
    std::fs::create_dir_all(&directory).map_err(|error| {
        err!("unable to create {}: {error}", directory.display())
    })?;
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
            lock_file(&root, "0.14.1/../x").unwrap().file_name().unwrap(),
            "0.14.1_.._x.lock"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
