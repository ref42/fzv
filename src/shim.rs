//! Shim mode: make the active version take effect immediately, everywhere.
//!
//! `<root>\.fzv\bin` is the fzv entry in `PATH` and contains copies of fzv named
//! `zig.exe` and `zls.exe`. A shim finds its own directory (so no configuration
//! or state elsewhere is needed), reads `<root>\.fzv\active`, and hands over to
//! that version's real executable. That is what makes switching instant: `fzv
//! use` only writes one small file, and every terminal, IDE or build tool that
//! is already running picks it up on its next `zig` invocation.
//!
//! One thing that cannot be done from here is changing the environment of the
//! shell that ran fzv, so a terminal opened *before* the `PATH` entry existed
//! would not find `zig` yet. The shims are therefore also written next to the
//! running fzv executable - a directory the user is already invoking fzv from,
//! which by definition is on `PATH` in that very terminal. Those copies are not
//! inside a versions directory, so they take the versions directory from the
//! shim entry in the user `PATH` (see [`target_for_shim`]).

use crate::error::{Result, err};
use crate::path_util::{self, PathStyle};
use crate::platform;
use crate::store;
use crate::version::Version;
use std::io;
use std::path::{Path, PathBuf};

/// Whether shims are installed for `root` (the state file marks the mode).
pub fn is_installed(root: &Path) -> bool {
    store::active_file(root).is_file() || store::shim_dir(root).is_dir()
}

/// The versions root a shim directory belongs to: `<root>\.fzv\bin` becomes `<root>`.
pub fn root_of_shim_dir(directory: &Path) -> Option<PathBuf> {
    let state = directory.parent()?;
    if !same_name(directory.file_name()?, "bin") || !same_name(state.file_name()?, store::STATE_DIR)
    {
        return None;
    }
    Some(state.parent()?.to_path_buf())
}

/// The versions root a shim executable belongs to.
pub fn root_of_shim(shim: &Path) -> Option<PathBuf> {
    root_of_shim_dir(shim.parent()?)
}

fn same_name(name: &std::ffi::OsStr, expected: &str) -> bool {
    let style = PathStyle::windows();
    let name = name.to_string_lossy();
    path_util::path_key(&name, style) == path_util::path_key(expected, style)
}

/// The active version recorded for `root`.
pub fn active_version(root: &Path) -> Result<Option<Version>> {
    let path = store::active_file(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            Version::parse(text).map(Some).ok_or_else(|| {
                err!(
                    "'{}' records an invalid Zig version; run 'fzv use' to select one",
                    path.display()
                )
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Records the active version.
///
/// The value is written to a temporary file and swapped in atomically, so a shim
/// starting at the same moment never reads a half-written or missing file.
pub fn set_active(root: &Path, version: &Version) -> Result<()> {
    let path = store::active_file(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| err!("unable to create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, format!("{version}\n"))
        .map_err(|error| err!("unable to write {}: {error}", temporary.display()))?;
    platform::replace_file(&temporary, &path)
}

/// Forgets the active version (the shims stay installed).
pub fn clear_active(root: &Path) -> Result<()> {
    let path = store::active_file(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The executable a shim hands over to.
///
/// `zig` follows the recorded version; `zls` is a single shared installation.
/// A shim inside a versions directory knows that directory from its own
/// location; a shim next to the launcher executable (which is outside any
/// versions directory) takes it from the shim entry in the user `PATH`, which is
/// where fzv records the selection.
pub fn target_for_shim(tool: &str, shim: &Path) -> Result<PathBuf> {
    let root = match root_of_shim(shim) {
        Some(root) => root,
        None => platform::shim_root_in_path()?.ok_or_else(|| {
            err!("no versions directory is known; run 'fzv use <version> --path DIR' to select one")
        })?,
    };
    if tool == "zls" {
        let executable = root.join("zls").join(platform::zls_executable());
        if !executable.is_file() {
            return Err(err!(
                "ZLS is not installed in {}; run 'fzv use <version>' to fetch it",
                root.display()
            ));
        }
        return Ok(executable);
    }
    let version = active_version(&root)?.ok_or_else(|| {
        err!(
            "no active Zig version recorded in {}; run 'fzv use <version>'",
            root.display()
        )
    })?;
    let executable = root.join(version.as_str()).join(platform::zig_executable());
    if !executable.is_file() {
        return Err(err!(
            "the active Zig version {version} is missing ({}); run 'fzv use <version>' to reinstall it",
            executable.display()
        ));
    }
    Ok(executable)
}

/// Writes (or refreshes) the `zig`/`zls` shims from this executable.
///
/// `force` copies unconditionally; otherwise the shims are only rewritten when
/// their size differs, which is enough to notice a rebuilt fzv without hashing a
/// ten megabyte binary on every `fzv use`.
pub fn install_shims(root: &Path, force: bool) -> Result<Vec<PathBuf>> {
    install_shims_into(&store::shim_dir(root), force)
}

/// Writes (or refreshes) the `zig`/`zls` shims in `directory`.
fn install_shims_into(directory: &Path, force: bool) -> Result<Vec<PathBuf>> {
    let running = std::env::current_exe()
        .map_err(|error| err!("unable to locate the fzv executable: {error}"))?;
    std::fs::create_dir_all(directory)
        .map_err(|error| err!("unable to create {}: {error}", directory.display()))?;
    let mut written = Vec::new();
    for name in ["zig", "zls"] {
        let target = directory.join(format!("{name}.exe"));
        if force || !is_current(&running, &target) {
            std::fs::copy(&running, &target).map_err(|error| {
                err!(
                    "unable to write the {name} shim ({}): {error}",
                    target.display()
                )
            })?;
        }
        written.push(target);
    }
    Ok(written)
}

/// The directory the shims should additionally be written to, so that the
/// terminal that ran fzv finds `zig` without picking up the `PATH` entry first.
///
/// That is the directory holding the running fzv executable, and only when it is
/// a directory `PATH` already reaches: anywhere else the copies would be useless
/// clutter. A launcher that is itself inside one of fzv's shim directories needs
/// nothing extra, because its shims are already reachable by location.
pub fn launcher_shim_dir(running: &Path, on_path: bool) -> Option<PathBuf> {
    if !on_path {
        return None;
    }
    let directory = running
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    if root_of_shim_dir(directory).is_some() {
        return None;
    }
    Some(directory.to_path_buf())
}

/// Installs the shims next to the running fzv executable.
///
/// Best effort by design: a read-only or missing launcher directory only means
/// the user keeps the one-time `PATH` pickup.
pub fn install_launcher_shims() -> Option<PathBuf> {
    install_shims_beside(&std::env::current_exe().ok()?)
}

/// Installs the shims next to the executable at `running`.
///
/// The path is a parameter so that an update can install the shims of the binary
/// it just put in place, rather than of whatever process happens to be running.
pub fn install_shims_beside(running: &Path) -> Option<PathBuf> {
    let on_path = platform::process_path_contains(running.parent()?);
    let directory = launcher_shim_dir(running, on_path)?;
    install_shims_into(&directory, false).ok()?;
    Some(directory)
}

/// Removes the shims and the recorded version.
pub fn remove_shims(root: &Path) -> Result<()> {
    for path in [store::active_file(root), store::shim_dir(root)] {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(err!("unable to remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

fn is_current(source: &Path, target: &Path) -> bool {
    match (std::fs::metadata(source), std::fs::metadata(target)) {
        (Ok(source), Ok(target)) => source.len() == target.len(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-shim-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn install_fake(root: &Path, version: &str) -> PathBuf {
        let executable = root.join(version).join(platform::zig_executable());
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"exe").unwrap();
        executable
    }

    fn make_shim(root: &Path) -> PathBuf {
        let shim = store::shim_dir(root).join(platform::zig_executable());
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, b"shim").unwrap();
        shim
    }

    #[test]
    fn maps_a_shim_back_to_its_versions_root() {
        let root = temp_root("root");
        let shim = make_shim(&root);
        assert_eq!(root_of_shim(&shim), Some(root.clone()));
        // Case differences must not matter on Windows.
        let upper = store::shim_dir(&root).join("ZIG.EXE");
        assert_eq!(root_of_shim(&upper), Some(root.clone()));
        // Something that is not a shim.
        assert_eq!(root_of_shim(Path::new(r"D:\tools\zig.exe")), None);
        assert_eq!(root_of_shim(&root.join("0.16.0").join("zig.exe")), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolves_the_recorded_version_and_reports_missing_ones() {
        let root = temp_root("resolve");
        let shim = make_shim(&root);
        assert_eq!(active_version(&root).unwrap(), None);
        assert!(
            target_for_shim("zig", &shim).is_err(),
            "nothing recorded yet"
        );

        let executable = install_fake(&root, "0.16.0");
        set_active(&root, &Version::parse("0.16.0").unwrap()).unwrap();
        assert_eq!(
            active_version(&root)
                .unwrap()
                .map(|version| version.as_str().to_string()),
            Some("0.16.0".to_string())
        );
        assert_eq!(target_for_shim("zig", &shim).unwrap(), executable);

        // Switching is nothing but rewriting the state file.
        install_fake(&root, "0.17.0-dev.1+abc");
        set_active(&root, &Version::parse("0.17.0-dev.1+abc").unwrap()).unwrap();
        assert_eq!(
            active_version(&root)
                .unwrap()
                .map(|version| version.as_str().to_string()),
            Some("0.17.0-dev.1+abc".to_string())
        );

        // A recorded version that is not installed is reported clearly.
        set_active(&root, &Version::parse("0.15.0").unwrap()).unwrap();
        let error = target_for_shim("zig", &shim).unwrap_err();
        assert!(error.to_string().contains("0.15.0"), "{error}");

        // A corrupt state file is reported instead of being ignored.
        std::fs::write(store::active_file(&root), "not-a-version\n").unwrap();
        assert!(active_version(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installs_beside_the_launcher_only_when_that_helps() {
        // A launcher in a directory this terminal's PATH reaches: that is where
        // the shims have to go, because fzv cannot change the shell.
        assert_eq!(
            launcher_shim_dir(Path::new(r"D:\RUST\.cargo\bin\fzv.exe"), true),
            Some(PathBuf::from(r"D:\RUST\.cargo\bin"))
        );
        // A directory PATH does not reach would only collect clutter.
        assert_eq!(
            launcher_shim_dir(Path::new(r"D:\build\fzv.exe"), false),
            None
        );
        // One of fzv's own shim directories needs no second copy.
        let root = temp_root("launcher");
        let running = store::shim_dir(&root).join("fzv.exe");
        assert_eq!(launcher_shim_dir(&running, true), None);
        assert_eq!(launcher_shim_dir(Path::new("fzv.exe"), true), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installs_and_removes_the_mode() {
        let root = temp_root("install");
        assert!(!is_installed(&root));
        let shims = install_shims(&root, true).unwrap();
        assert_eq!(shims.len(), 2);
        for shim in &shims {
            assert!(shim.is_file(), "{} was not written", shim.display());
        }
        set_active(&root, &Version::parse("0.16.0").unwrap()).unwrap();
        assert!(is_installed(&root));

        // Refreshing is a no-op when the sizes match, and does not fail.
        install_shims(&root, false).unwrap();

        remove_shims(&root).unwrap();
        assert!(!is_installed(&root));
        assert!(!store::shim_dir(&root).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
