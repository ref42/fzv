//! Installing a Zig version (and ZLS) into a versions directory.
//!
//! An install is idempotent and resumable: the archive is downloaded next to the
//! version directory, verified against the published checksum, unpacked, and then
//! flattened so that `zig` sits directly in `<versions>/<version>`. A lock file
//! serialises concurrent installs of the same target, so two fzv processes cannot
//! write into the same download or directory at once.

pub mod archive;
pub mod checksum;

use crate::error::{Result, err};
use crate::index;
use crate::layout;
use crate::log::detail;
use crate::platform;
use crate::progress::Spinner;
use crate::store;
use crate::version::Version;
use std::path::{Path, PathBuf};

pub use checksum::verify_sha256;

/// Installs `version` below `root` if needed and returns its executable.
pub fn ensure_zig(root: &Path, version: &Version) -> Result<PathBuf> {
    let directory = root.join(version.as_str());
    let executable = platform::zig_executable_in(&directory);
    if executable.is_file() {
        return Ok(executable);
    }
    let _lock = InstallLock::acquire(root, version.as_str())?;
    // Zig's ZIP archives contain a versioned top-level directory, while its tar
    // archives are flattened; reuse either layout.
    if directory.is_dir()
        && let Ok(existing) = layout::find_zig_executable(&directory)
    {
        return layout::flatten_install(&directory, &existing);
    }
    let archive = index::download_info(version, root)?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| err!("unable to create {}: {error}", directory.display()))?;
    let archive_path = directory.join(&archive.file_name);
    if archive_path.is_file() {
        detail!("fzv: reusing downloaded Zig archive");
        if let Err(error) = verify_sha256(&archive_path, archive.sha256.as_deref()) {
            eprintln!("fzv: {error}; downloading it again");
            std::fs::remove_file(&archive_path)
                .map_err(|error| err!("unable to remove {}: {error}", archive_path.display()))?;
        }
    }
    if !archive_path.is_file() {
        crate::download::download(&archive.url, &archive_path, &format!("Zig {version}"))?;
        if let Err(error) = verify_sha256(&archive_path, archive.sha256.as_deref()) {
            // Never leave bytes that failed verification behind.
            let _ = std::fs::remove_file(&archive_path);
            return Err(error);
        }
    }
    let _unpacking = Spinner::start(&format!("unpacking Zig {version}"));
    archive::extract_archive(&archive_path, &directory)?;
    let extracted = layout::find_zig_executable(&directory)?;
    let _ = std::fs::remove_file(&archive_path);
    layout::flatten_install(&directory, &extracted)
}

/// Installs ZLS below `root` if needed and returns its executable.
pub fn ensure_zls(root: &Path) -> Result<PathBuf> {
    let directory = root.join("zls");
    let executable = directory.join(platform::zls_executable());
    if executable.is_file() {
        return Ok(executable);
    }
    let _lock = InstallLock::acquire(root, "zls")?;
    if directory.is_dir()
        && let Ok(existing) = layout::find_named_executable(&directory, "zls")
    {
        return layout::flatten_install(&directory, &existing);
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| err!("unable to create {}: {error}", directory.display()))?;
    let zls_arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err(err!("unsupported CPU architecture for ZLS"));
    };
    let archive = format!("zls-{zls_arch}-windows.zip");
    let url = format!("https://github.com/zigtools/zls/releases/latest/download/{archive}");
    let archive_path = directory.join(&archive);
    if !archive_path.is_file() {
        // ZLS releases do not publish a checksum, so this download cannot be
        // verified the way Zig archives are.
        detail!("fzv: downloading ZLS (no published checksum; not verifying)");
        let mut last_error = None;
        for attempt in 1..=3 {
            match crate::download::download(&url, &archive_path, "ZLS") {
                Ok(()) => break,
                Err(error) => {
                    if attempt < 3 {
                        eprintln!("fzv: ZLS download attempt {attempt} failed; retrying...");
                        std::thread::sleep(std::time::Duration::from_millis(500 * attempt));
                    }
                    last_error = Some(error);
                }
            }
        }
        if !archive_path.is_file() {
            let error = last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(err!("unable to download ZLS after 3 attempts: {error}"));
        }
    }
    let _unpacking = Spinner::start("unpacking ZLS");
    archive::extract_archive(&archive_path, &directory)?;
    let extracted = layout::find_named_executable(&directory, "zls")?;
    let _ = std::fs::remove_file(archive_path);
    layout::flatten_install(&directory, &extracted)
}

/// Serialises installs of one target.
struct InstallLock {
    file: std::fs::File,
    path: PathBuf,
}

impl InstallLock {
    fn acquire(root: &Path, name: &str) -> Result<InstallLock> {
        let path = store::lock_file(root, name)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| err!("unable to open {}: {error}", path.display()))?;
        file.try_lock()
            .map_err(|error| err!("another fzv process is already installing {name} ({error})"))?;
        Ok(InstallLock { file, path })
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-install-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn locks_are_exclusive_and_released_on_drop() {
        let root = temp_dir("lock");
        let first = InstallLock::acquire(&root, "0.14.1").unwrap();
        assert!(
            InstallLock::acquire(&root, "0.14.1").is_err(),
            "the same target must not be installed twice at once"
        );
        // A different target is unaffected.
        let other = InstallLock::acquire(&root, "0.15.0").unwrap();
        drop(first);
        drop(other);
        // Released on drop, and the lock file is cleaned up.
        let again = InstallLock::acquire(&root, "0.14.1").unwrap();
        drop(again);
        assert!(root.join(".fzv").join("locks").is_dir());
        assert!(!root.join(".fzv").join("locks").join("0.14.1.lock").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_already_installed_version_is_returned_without_downloading() {
        let root = temp_dir("already");
        let directory = root.join("0.14.1");
        std::fs::create_dir_all(&directory).unwrap();
        let executable = platform::zig_executable_in(&directory);
        std::fs::write(&executable, b"exe").unwrap();
        assert_eq!(
            ensure_zig(&root, &Version::parse("0.14.1").unwrap()).unwrap(),
            executable
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
