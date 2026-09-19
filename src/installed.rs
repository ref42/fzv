//! Versions installed in a versions directory.
//!
//! A version directory is named after the version it holds, which is what lets
//! fzv recognise the active version from `PATH` alone. Directories that are not
//! versions are ignored — including fzv's own `.fzv` state directory — while a
//! version directory whose executable is missing is still reported, so that it
//! can be inspected or removed instead of silently disappearing.

use crate::error::{Result, err};
use crate::layout;
use crate::version::{Version, sort_desc};
use std::path::{Path, PathBuf};

/// A directory found in the versions directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    /// The directory name, which is also the selector that addresses it.
    pub name: String,
    /// The parsed version, or `None` for the legacy `dev` directory that older
    /// fzv builds used for snapshots.
    pub version: Option<Version>,
    /// Whether a Zig executable was found inside.
    pub ready: bool,
}

impl InstalledVersion {
    pub fn directory(&self, root: &Path) -> PathBuf {
        root.join(&self.name)
    }
}

/// Lists the version directories below `root`, newest first.
pub fn scan(root: &Path) -> Result<Vec<InstalledVersion>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|error| err!("unable to read {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| err!("unable to read an entry: {error}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let version = Version::parse(name);
        if version.is_none() && name != "dev" {
            continue;
        }
        versions.push(InstalledVersion {
            name: name.to_string(),
            version,
            ready: layout::find_zig_executable(&path).is_ok(),
        });
    }
    versions.sort_by(|left, right| {
        right
            .version
            .cmp(&left.version)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(versions)
}

/// The versions a user can select, newest first.
pub fn selectable(root: &Path) -> Result<Vec<Version>> {
    let mut versions: Vec<Version> = scan(root)?
        .into_iter()
        .filter(|installed| installed.ready)
        .filter_map(|installed| installed.version)
        .collect();
    sort_desc(&mut versions);
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::executable_name;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-installed-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn lists_versions_newest_first_and_flags_incomplete_ones() {
        let root = temp_dir("scan");
        for (name, executable) in [
            ("0.13.0", true),
            ("0.14.1", true),
            ("0.15.0", false),
            ("dev", true),
        ] {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory).unwrap();
            if executable {
                std::fs::write(directory.join(executable_name("zig")), b"exe").unwrap();
            }
        }
        // fzv's own state directory and unrelated directories are ignored.
        std::fs::create_dir_all(root.join(".fzv").join("locks")).unwrap();
        std::fs::create_dir_all(root.join("zls")).unwrap();
        std::fs::write(root.join("README"), b"x").unwrap();

        let scanned = scan(&root).unwrap();
        let names: Vec<&str> = scanned
            .iter()
            .map(|installed| installed.name.as_str())
            .collect();
        assert_eq!(names, ["0.15.0", "0.14.1", "0.13.0", "dev"]);

        assert!(!scanned[0].ready, "0.15.0 has no executable");
        assert!(scanned[1].ready);
        assert_eq!(scanned[3].version, None, "legacy dev directory");

        // Only usable, parseable versions can be selected.
        let selectable: Vec<String> = super::selectable(&root)
            .unwrap()
            .iter()
            .map(|version| version.as_str().to_string())
            .collect();
        assert_eq!(selectable, ["0.14.1", "0.13.0"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_directory_lists_nothing() {
        let root = temp_dir("scan-missing").join("nope");
        assert!(scan(&root).unwrap().is_empty());
        assert!(super::selectable(&root).unwrap().is_empty());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
