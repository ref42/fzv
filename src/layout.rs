//! Finding the Zig executable inside an installed version directory.
//!
//! Zig's own archives and ZLS's archives disagree about whether the files sit in
//! a versioned top-level directory or at the root, so fzv searches for the
//! executable and then flattens whatever directory it found into place. The
//! search never follows symlinks (an archive must not be able to make it loop)
//! and never descends further than [`MAX_DEPTH`].

use crate::error::{Result, err};
use std::path::{Path, PathBuf};

/// How deep an archive may nest the executable it ships.
const MAX_DEPTH: usize = 8;

/// Finds `name` (with the platform's executable suffix) below `root`.
pub fn find_named_executable(root: &Path, name: &str) -> Result<PathBuf> {
    find_at(root, &executable_name(name), 0)
}

/// Finds the Zig executable below `root`.
pub fn find_zig_executable(root: &Path) -> Result<PathBuf> {
    find_named_executable(root, "zig")
}

/// The file name of `name` on this platform.
pub fn executable_name(name: &str) -> String {
    format!("{name}.exe")
}

fn find_at(directory: &Path, wanted: &str, depth: usize) -> Result<PathBuf> {
    for entry in std::fs::read_dir(directory)
        .map_err(|error| err!("unable to read {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| err!("unable to read an entry: {error}"))?;
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(wanted) {
            return Ok(path);
        }
        // `file_type` does not follow symlinks, so an archive cannot make this
        // search walk in circles.
        if depth < MAX_DEPTH
            && entry
                .file_type()
                .map_err(|error| err!("unable to inspect an entry: {error}"))?
                .is_dir()
            && let Ok(found) = find_at(&path, wanted, depth + 1)
        {
            return Ok(found);
        }
    }
    Err(err!("archive did not contain {wanted}"))
}

/// Moves a nested installation up to `root`, replacing existing entries instead
/// of discarding the freshly extracted files, and verifies the result.
pub fn flatten_install(root: &Path, executable: &Path) -> Result<PathBuf> {
    if executable.parent() == Some(root) {
        return Ok(executable.to_path_buf());
    }
    let source = executable
        .parent()
        .ok_or_else(|| err!("installed executable has no parent directory"))?;
    let executable_name = executable
        .file_name()
        .ok_or_else(|| err!("installed executable has no file name"))?
        .to_owned();
    let entries: Vec<_> = std::fs::read_dir(source)
        .map_err(|error| err!("unable to read {}: {error}", source.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| err!("unable to read {}: {error}", source.display()))?;
    for entry in entries {
        move_entry(&entry.path(), &root.join(entry.file_name()))?;
    }
    // Drop the now-empty staging directory and any empty ancestor it left
    // behind, without ever touching `root` itself.
    let mut directory = Some(source.to_path_buf());
    while let Some(current) = directory {
        if current == root {
            break;
        }
        if std::fs::remove_dir(&current).is_err() {
            break;
        }
        directory = current.parent().map(Path::to_path_buf);
    }
    let flattened = root.join(&executable_name);
    if !flattened.is_file() {
        return Err(err!(
            "installation is incomplete: {} does not exist",
            flattened.display()
        ));
    }
    Ok(flattened)
}

/// Moves `from` onto `to`, merging directories and replacing files.
fn move_entry(from: &Path, to: &Path) -> Result<()> {
    // `symlink_metadata` does not follow links, so a symlinked directory is
    // moved as a link instead of being merged into.
    let metadata = std::fs::symlink_metadata(from)
        .map_err(|error| err!("unable to inspect {}: {error}", from.display()))?;
    if metadata.is_dir() && to.is_dir() {
        let entries: Vec<_> = std::fs::read_dir(from)
            .map_err(|error| err!("unable to read {}: {error}", from.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| err!("unable to read {}: {error}", from.display()))?;
        for entry in entries {
            move_entry(&entry.path(), &to.join(entry.file_name()))?;
        }
        return std::fs::remove_dir(from)
            .map_err(|error| err!("unable to remove {}: {error}", from.display()));
    }
    if to.exists() {
        remove_path(to)?;
    }
    std::fs::rename(from, to)
        .map_err(|error| err!("unable to move {} into place: {error}", from.display()))
}

/// Removes a file or directory without following a symlink or junction.
fn remove_path(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| err!("unable to inspect {}: {error}", path.display()))?;
    let result = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|error| err!("unable to remove {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-layout-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn zig_name() -> &'static str {
        if cfg!(windows) { "zig.exe" } else { "zig" }
    }

    #[test]
    fn finds_executables_recursively_but_not_through_symlinks() {
        let root = temp_dir("find");
        let nested = root.join("zig-x86_64-0.14.1");
        std::fs::create_dir_all(&nested).unwrap();
        let executable = nested.join(zig_name());
        std::fs::write(&executable, b"test").unwrap();
        assert_eq!(find_zig_executable(&root).unwrap(), executable);

        std::fs::remove_dir_all(nested).unwrap();
        assert!(find_zig_executable(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flattening_replaces_stale_files_instead_of_the_new_ones() {
        let root = temp_dir("flatten");
        let nested = root.join("zig-x86_64-windows-0.14.1");
        std::fs::create_dir_all(nested.join("lib")).unwrap();
        let executable = nested.join(zig_name());
        std::fs::write(&executable, b"new-exe").unwrap();
        std::fs::write(nested.join("lib").join("std.zig"), b"new-lib").unwrap();
        // A previously interrupted install left stale files at the top level.
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib").join("std.zig"), b"stale-lib").unwrap();
        std::fs::write(root.join("leftover.txt"), b"leftover").unwrap();

        let flattened = flatten_install(&root, &executable).unwrap();
        assert_eq!(flattened, root.join(zig_name()));
        assert_eq!(std::fs::read(&flattened).unwrap(), b"new-exe");
        assert_eq!(
            std::fs::read(root.join("lib").join("std.zig")).unwrap(),
            b"new-lib"
        );
        assert!(!nested.exists());
        // Files the archive does not carry are left alone.
        assert!(root.join("leftover.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flattening_cleans_empty_ancestors_but_not_the_root() {
        let root = temp_dir("flatten-deep");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let executable = nested.join(zig_name());
        std::fs::write(&executable, b"exe").unwrap();
        let flattened = flatten_install(&root, &executable).unwrap();
        assert_eq!(std::fs::read(&flattened).unwrap(), b"exe");
        assert!(!root.join("a").exists());
        assert!(root.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }
}
