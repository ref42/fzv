//! Unix activation: point a symlink on `PATH` at the selected version.
//!
//! A child process cannot change its parent shell's environment, so instead of
//! rewriting `PATH` (as the Windows backend does) fzv maintains a single `zig`
//! symlink in a directory the user already has on `PATH`. The active version is
//! then still derived from `PATH` — by resolving that link — and fzv needs no
//! state file.
//!
//! The symlink is only ever replaced when it is a symlink, and only removed when
//! it points inside the versions directory, so a `zig` the user installed by
//! hand is never overwritten or deleted.

use super::{Activation, zig_executable};
use crate::error::{Result, err};
use crate::path_util::{self, PathStyle};
use crate::version::Version;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// The directory holding fzv's symlinks.
pub fn bin_dir() -> Result<PathBuf> {
    for variable in ["FZV_BIN_DIR", "XDG_BIN_HOME"] {
        if let Some(value) = std::env::var_os(variable) {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                return Ok(path);
            }
            return Err(err!(
                "{variable} is set to '{}', which is not an absolute path",
                path.display()
            ));
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| err!("HOME is not set to an absolute path"))?;
    Ok(home.join(".local/bin"))
}

fn style() -> PathStyle {
    PathStyle::unix()
}

fn link_path() -> Result<PathBuf> {
    Ok(bin_dir()?.join(zig_executable()))
}

fn link_target(link: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(link).ok()?;
    if target.is_absolute() {
        Some(target)
    } else {
        Some(link.parent()?.join(target))
    }
}

/// The version directory the `zig` link points into, falling back to a version
/// directory placed directly on `PATH`.
pub fn active_zig_dir() -> Result<Option<PathBuf>> {
    if let Ok(link) = link_path()
        && let Some(directory) = link_target(&link).and_then(|target| {
            target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
    {
        return Ok(Some(directory));
    }
    Ok(crate::platform::zig_dir_in_path(
        &std::env::var("PATH").unwrap_or_default(),
    ))
}

pub fn activate(root: &Path, version: &Version) -> Result<Activation> {
    let directory = root.join(version.as_str());
    let target = super::zig_executable_in(&directory);
    if !target.is_file() {
        return Err(err!(
            "cannot activate Zig {version}; {} does not exist",
            target.display()
        ));
    }
    let bin = bin_dir()?;
    std::fs::create_dir_all(&bin)
        .map_err(|error| err!("unable to create {}: {error}", bin.display()))?;
    let link = bin.join(zig_executable());

    let mut notes = Vec::new();
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if let Some(previous) = link_target(&link)
                && previous != target
            {
                notes.push(format!(
                    "replaced the existing {} -> {}",
                    link.display(),
                    previous.display()
                ));
            }
            std::fs::remove_file(&link)
                .map_err(|error| err!("unable to replace {}: {error}", link.display()))?;
        }
        Ok(_) => {
            return Err(err!(
                "refusing to replace {}; it is not a symlink fzv created",
                link.display()
            ));
        }
        Err(_) => {}
    }
    symlink(&target, &link)
        .map_err(|error| err!("unable to create {}: {error}", link.display()))?;

    if !path_contains(&bin) {
        notes.push(format!(
            "{} is not on PATH yet; add it with: export PATH=\"{}:$PATH\"",
            bin.display(),
            bin.display()
        ));
    }
    Ok(Activation { directory, notes })
}

/// Removes the link again, but only when it points inside `root`.
pub fn deactivate(root: &Path) -> Result<()> {
    let Ok(link) = link_path() else {
        return Ok(());
    };
    let Some(target) = link_target(&link) else {
        return Ok(());
    };
    let scope = path_util::path_scope(&path_util::path_key(&root.to_string_lossy(), style()), style());
    let target_key = path_util::path_key(&target.to_string_lossy(), style());
    if scope.is_some_and(|scope| target_key.starts_with(&scope)) {
        std::fs::remove_file(&link)
            .map_err(|error| err!("unable to remove {}: {error}", link.display()))?;
    }
    Ok(())
}

fn path_contains(directory: &Path) -> bool {
    let value = std::env::var("PATH").unwrap_or_default();
    let wanted = path_util::path_key(&directory.to_string_lossy(), style());
    value
        .split(style().separator)
        .any(|entry| path_util::path_key(entry, style()) == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The link plumbing is exercised through a temporary bin directory, which
    /// keeps the test independent of the real `~/.local/bin`.
    #[test]
    fn refuses_to_replace_a_regular_file() {
        let base = std::env::temp_dir().join(format!("fzv-unix-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A hand-written `zig` that fzv must not touch.
        let link = base.join(zig_executable());
        std::fs::write(&link, b"#!/bin/sh\n").unwrap();

        // SAFETY: single-threaded test process section; the variable is restored
        // right after use.
        unsafe {
            std::env::set_var("FZV_BIN_DIR", &base);
        }
        let root = base.join("versions");
        let version = Version::parse("0.14.1").unwrap();
        let error = activate(&root, &version).unwrap_err();
        assert!(error.to_string().contains("refusing to replace"), "{error}");
        unsafe {
            std::env::remove_var("FZV_BIN_DIR");
        }
        std::fs::remove_dir_all(base).unwrap();
    }
}
