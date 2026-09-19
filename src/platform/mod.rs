//! Platform integration.
//!
//! Everything that differs between operating systems lives here: the names of
//! the Zig executables (and of the environment), and — most importantly — how
//! the *active* version is recorded in `PATH`.
//!
//! The invariant the rest of the crate relies on is deliberately narrow:
//!
//! * **Windows** rewrites `HKCU\Environment\Path`, so the version directory
//!   itself is the `PATH` entry.
//! * **Unix** cannot change a parent shell's environment, so `fzv` keeps a
//!   `zig` symlink in a directory the user already has on `PATH`
//!   (`$XDG_BIN_HOME`, falling back to `~/.local/bin`).
//!
//! Either way the active version is derived from `PATH` and never from a state
//! file of fzv's own.

use crate::path_util::{self, PathStyle};
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{activate, active_zig_dir, deactivate};
#[cfg(windows)]
pub use windows::{activate, active_zig_dir, deactivate};

/// What making a version active changed.
#[derive(Debug)]
pub struct Activation {
    /// The directory that now holds the active Zig executable.
    pub directory: PathBuf,
    /// Lines the CLI should show, already phrased for the user.
    pub notes: Vec<String>,
}

/// The conventions of the platform fzv is running on.
pub fn style() -> PathStyle {
    PathStyle::current()
}

/// Name of the Zig executable on this platform.
pub fn zig_executable() -> &'static str {
    if cfg!(windows) { "zig.exe" } else { "zig" }
}

/// Name of the ZLS executable on this platform.
pub fn zls_executable() -> &'static str {
    if cfg!(windows) { "zls.exe" } else { "zls" }
}

/// The exit code to report for a finished child process, following the shell's
/// convention for signals on Unix.
pub fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// The first `PATH` entry that is one of fzv's Zig directories.
pub fn zig_dir_in_path(value: &str) -> Option<PathBuf> {
    path_util::zig_dir_in_path(value, style(), zig_executable())
}

/// The Zig executable inside a version directory.
pub fn zig_executable_in(directory: &Path) -> PathBuf {
    directory.join(zig_executable())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-platform-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn recognises_its_own_directories_and_nothing_else() {
        let base = temp_dir("detect");
        let zig_dir = base.join("zig").join("0.14.1");
        std::fs::create_dir_all(&zig_dir).unwrap();
        std::fs::write(zig_executable_in(&zig_dir), b"exe").unwrap();
        // An unrelated tool in a version-named directory.
        let ninja = base.join("ninja").join("1.13.2");
        std::fs::create_dir_all(&ninja).unwrap();
        std::fs::write(ninja.join("ninja.exe"), b"exe").unwrap();

        let value = format!(
            "{}{}{}{}{}",
            base.join("tools").display(),
            style().separator,
            ninja.display(),
            style().separator,
            zig_dir.display()
        );
        assert_eq!(zig_dir_in_path(&value), Some(zig_dir));
        assert_eq!(
            zig_dir_in_path(&format!("{}{}", base.join("tools").display(), "")),
            None
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
