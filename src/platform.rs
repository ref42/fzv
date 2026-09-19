//! Windows integration: the Zig executables and the user `PATH`.
//!
//! The active version *is* the Zig directory in `HKCU\Environment\Path`, so fzv
//! keeps no record of it anywhere else: `fzv use` replaces that one entry, and
//! every other command derives what is active by looking at `PATH`.
//!
//! The rewrite is deliberately paranoid, because it edits a value the whole
//! machine depends on:
//!
//! * a failed read aborts instead of being treated as an empty `PATH`;
//! * the registry value type (`REG_SZ` / `REG_EXPAND_SZ`) is preserved;
//! * only directories recognised as fzv's own are removed;
//! * the written value is read back and compared before reporting success;
//! * running programs are told that the environment changed.

use crate::error::{Result, err};
use crate::path_util::{self, PathStyle};
use crate::version::Version;
use std::io;
use std::path::{Path, PathBuf};

const ENVIRONMENT_KEY: &str = "Environment";
const PATH_VALUE: &str = "Path";

/// What making a version active changed.
#[derive(Debug)]
pub struct Activation {
    /// The version directory that now holds the active Zig executable.
    pub zig_directory: PathBuf,
    /// The shared ZLS directory, when ZLS is installed.
    pub zls_directory: Option<PathBuf>,
    /// Whether the user `PATH` had to be pointed at the shims (only the first
    /// time a versions directory is used).
    pub path_changed: bool,
    /// Whether `zig` is reachable in the terminal that ran fzv, without that
    /// terminal picking up the `PATH` entry first.
    pub immediate: bool,
    /// Lines the CLI should show, already phrased for the user.
    pub notes: Vec<String>,
}

/// The `PATH` conventions of the platform fzv is built for.
pub fn style() -> PathStyle {
    PathStyle::windows()
}

/// Name of the Zig executable.
pub fn zig_executable() -> &'static str {
    "zig.exe"
}

/// Name of the ZLS executable.
pub fn zls_executable() -> &'static str {
    "zls.exe"
}

/// The Zig executable inside a version directory.
pub fn zig_executable_in(directory: &Path) -> PathBuf {
    directory.join(zig_executable())
}

/// The exit code to report for a finished child process.
pub fn exit_code(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// The first `PATH` entry that is one of fzv's Zig directories.
pub fn zig_dir_in_path(value: &str) -> Option<PathBuf> {
    path_util::zig_dir_in_path(value, style(), is_fzv_version_dir)
}

/// Replaces `destination` with `source` in one step.
///
/// `std::fs::rename` refuses to overwrite on Windows, and removing the old file
/// first would leave a moment where the file does not exist — which a shim
/// starting at that moment would see. `MoveFileExW` swaps them atomically
/// instead, which matters because shims read this file while `fzv use` writes it.
pub fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;

    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let source = wide(source);
    let destination_wide = wide(destination);
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    if replaced == 0 {
        return Err(err!(
            "unable to replace {}: {}",
            destination.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Whether the user `PATH` contains `directory`.
pub fn user_path_contains(directory: &Path) -> Result<bool> {
    Ok(path_util::contains_entry(
        &read_user_path()?,
        directory,
        style(),
    ))
}

/// Whether the environment fzv inherited already reaches `directory`.
///
/// This is the `PATH` of the shell that started fzv, so it answers "can this
/// terminal run what fzv just installed?".
pub fn process_path_contains(directory: &Path) -> bool {
    std::env::var("PATH").is_ok_and(|value| path_util::contains_entry(&value, directory, style()))
}

/// The versions root selected by the shims in the user `PATH`, if any.
///
/// With shims installed, `PATH` holds `<root>\.fzv\bin` instead of a version
/// directory, so this is how the versions directory is found again.
pub fn shim_root_in_path() -> Result<Option<PathBuf>> {
    for entry in read_user_path()?.split(style().separator).map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        let text = path_util::strip_verbatim(entry, style());
        if let Some(root) = crate::shim::root_of_shim_dir(Path::new(&text)) {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

impl Activation {
    /// The shim directory that makes this selection reachable.
    pub fn shim_directory(&self, root: &Path) -> PathBuf {
        crate::store::shim_dir(root)
    }
}

/// The `PATH` value the calling shell should adopt to use the current selection
/// right away.
///
/// A child process cannot change its parent's environment, so this is *returned*
/// rather than applied: the shell assigns it (`$env:PATH = (fzv use <v>
/// --print-path)`). It is derived from the environment fzv inherited, so entries
/// that only exist in that session (a toolchain prompt, a virtualenv) survive
/// while the previous fzv entries are replaced.
pub fn session_path(root: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    path_util::rewrite_path(
        &current,
        root,
        &[crate::store::shim_dir(root).as_path()],
        style(),
        is_fzv_directory,
    )
}

/// Whether `directory` is one of the version directories fzv creates.
///
/// An install that was interrupted can leave the directory empty, or with the
/// archive still nested inside it, so the executable alone is not enough: a
/// directory named after a version whose parent is one of fzv's versions
/// directories (it holds fzv's `.fzv` state) is recognised as well. That is what
/// keeps the versions directory derivable when an install needs repairing.
fn is_fzv_version_dir(directory: &Path) -> bool {
    if !path_util::is_version_dir_name(&path_util::path_key(&directory.to_string_lossy(), style()))
    {
        return false;
    }
    crate::layout::find_zig_executable(directory).is_ok()
        || directory
            .parent()
            .is_some_and(crate::store::is_versions_root)
}

/// Whether `directory` is the shared ZLS directory fzv creates.
fn is_fzv_zls_dir(directory: &Path) -> bool {
    if !path_util::is_zls_dir_name(&path_util::path_key(&directory.to_string_lossy(), style())) {
        return false;
    }
    directory.join(zls_executable()).is_file()
        || directory
            .parent()
            .is_some_and(crate::store::is_versions_root)
}

/// Any `PATH` entry fzv owns: one of its version directories or its ZLS one.
fn is_fzv_directory(directory: &Path) -> bool {
    is_fzv_version_dir(directory) || is_fzv_zls_dir(directory)
}

/// The Zig version directory fzv put in the user `PATH`, if any.
pub fn active_zig_dir() -> Result<Option<PathBuf>> {
    Ok(zig_dir_in_path(&read_user_path()?))
}

/// Makes `version` the active selection.
///
/// This is one small file write: fzv keeps copies of itself named `zig.exe` and
/// `zls.exe` in `<root>\.fzv\bin`, that directory is the single fzv entry in the
/// user `PATH`, and the shims follow `<root>\.fzv\active`. Recording the version
/// is therefore all it takes for every terminal, IDE and build tool to use it on
/// their next `zig` invocation — and `PATH` is only touched if it does not point
/// at the shims yet.
/// Makes `version` the active selection.
///
/// This is one small file write: fzv keeps copies of itself named `zig.exe` and
/// `zls.exe` in `<root>\.fzv\bin`, that directory is the fzv entry in the user
/// `PATH`, and the shims follow `<root>\.fzv\active`. Recording the version is
/// therefore all it takes for every terminal, IDE and build tool to use it on
/// their next `zig` invocation — and `PATH` is only touched if it does not point
/// at the shims yet.
///
/// The shims are also written next to the running fzv executable ([`crate::shim`]
/// explains why), which is what makes the terminal that ran fzv work straight
/// away: [`Activation::immediate`] records whether that succeeded.
pub fn activate(root: &Path, version: &Version) -> Result<Activation> {
    let zig_directory = root.join(version.as_str());
    let executable = zig_executable_in(&zig_directory);
    if !executable.is_file() {
        return Err(err!(
            "cannot activate Zig {version}; {} does not exist",
            executable.display()
        ));
    }
    // ZLS is a single shared installation, so it follows whichever version is
    // active.
    let zls_directory = root.join("zls");
    let zls = zls_directory
        .join(zls_executable())
        .is_file()
        .then_some(zls_directory.clone());

    crate::shim::install_shims(root, false)?;
    crate::shim::set_active(root, version)?;
    let notes = point_path_at_shims(root)?;
    // Reachable right now when the terminal already had the shim directory, or
    // when the shims just landed in a directory it does have - typically the one
    // holding fzv.exe itself.
    let immediate = process_path_contains(&crate::store::shim_dir(root))
        || crate::shim::install_launcher_shims().is_some();
    Ok(Activation {
        zig_directory,
        zls_directory: zls,
        path_changed: !notes.is_empty(),
        immediate,
        notes,
    })
}

/// Makes the user `PATH` use the shims of `root`, and reports what changed.
///
/// Only called when the current `PATH` does not already end up at the shim
/// directory, so switching versions after the first time rewrites nothing.
fn point_path_at_shims(root: &Path) -> Result<Vec<String>> {
    let shim_directory = crate::store::shim_dir(root);
    if path_uses_shims(root, &shim_directory)? {
        return Ok(Vec::new());
    }
    let (_, dropped) = rewrite(&[shim_directory.as_path()], root)?;
    let mut notes: Vec<String> = dropped
        .into_iter()
        .map(|entry| format!("dropped stale PATH entry: {entry}"))
        .collect();
    notes.push(format!(
        "PATH now points at the shims in {}",
        shim_directory.display()
    ));
    Ok(notes)
}

/// Whether the user `PATH` already uses exactly the shims of `root`.
fn path_uses_shims(root: &Path, shim_directory: &Path) -> Result<bool> {
    let current = read_user_path()?;
    let rewritten =
        path_util::rewrite_path(&current, root, &[shim_directory], style(), is_fzv_directory);
    Ok(rewritten == current)
}

/// Forgets the active version. The shims and their `PATH` entry stay, so the
/// next `fzv use` needs no `PATH` write at all.
pub fn deactivate(root: &Path) -> Result<()> {
    crate::shim::clear_active(root)
}

/// Rewrites the user `PATH` for `root`, returning the saved value and the entries
/// that were dropped.
fn rewrite(wanted: &[&Path], root: &Path) -> Result<(String, Vec<String>)> {
    use winreg::{
        RegKey, RegValue,
        enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, REG_SZ},
    };

    let environment = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(ENVIRONMENT_KEY, KEY_READ | KEY_WRITE)
        .map_err(|error| err!("unable to open user environment variables: {error}"))?;
    let old_path = read_value(&environment)?;
    let old_type = environment
        .get_raw_value(PATH_VALUE)
        .map(|value| value.vtype)
        .unwrap_or(REG_SZ);
    let new_path = path_util::rewrite_path(&old_path, root, wanted, style(), is_fzv_directory);
    let dropped = path_util::dropped_entries(&old_path, &new_path, wanted, style());

    // Keep the registry value type (REG_EXPAND_SZ is common for PATH).
    let mut bytes: Vec<u8> = new_path.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.extend_from_slice(&[0, 0]);
    let value = RegValue {
        bytes: bytes.into(),
        vtype: if old_type == REG_EXPAND_SZ {
            REG_EXPAND_SZ
        } else {
            REG_SZ
        },
    };
    environment
        .set_raw_value(PATH_VALUE, &value)
        .map_err(|error| err!("unable to update the user PATH: {error}"))?;
    let saved: String = environment
        .get_value(PATH_VALUE)
        .map_err(|error| err!("unable to verify the user PATH: {error}"))?;
    if saved != new_path {
        return Err(err!("the user PATH was not saved as intended"));
    }
    broadcast_environment_change();
    Ok((saved, dropped))
}

/// Reads the user `PATH`.
///
/// A failed read must never be treated as "the PATH is empty": writing that back
/// would destroy the user's environment.
fn read_user_path() -> Result<String> {
    use winreg::{
        RegKey,
        enums::{HKEY_CURRENT_USER, KEY_READ},
    };

    let environment = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(ENVIRONMENT_KEY, KEY_READ)
        .map_err(|error| err!("unable to open user environment variables: {error}"))?;
    read_value(&environment)
}

fn read_value(environment: &winreg::RegKey) -> Result<String> {
    match environment.get_value::<String, _>(PATH_VALUE) {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(err!(
            "unable to read the user PATH ({error}); refusing to overwrite it"
        )),
    }
}

/// Tells running programs that the environment changed, so that newly started
/// terminals inherit the updated `PATH` without a re-login.
fn broadcast_environment_change() {
    const HWND_BROADCAST: isize = 0xffff;
    const WM_SETTINGCHANGE: u32 = 0x001a;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageTimeoutW(
            window: isize,
            message: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout: u32,
            result: *mut usize,
        ) -> isize;
    }

    let mut parameter: Vec<u16> = "Environment\0".encode_utf16().collect();
    let mut result = 0usize;
    // Best effort: the update is already committed either way.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            parameter.as_mut_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
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
        // Unrelated tools that also live in version-named directories, and a
        // version directory whose executable was deleted.
        let ninja = base.join("ninja").join("1.13.2");
        std::fs::create_dir_all(&ninja).unwrap();
        std::fs::write(ninja.join("ninja.exe"), b"exe").unwrap();
        let probe_rs = base.join("probe-rs").join("0.32.0");
        std::fs::create_dir_all(&probe_rs).unwrap();
        std::fs::write(probe_rs.join("probe-rs.exe"), b"exe").unwrap();
        let broken = base.join("zig").join("0.13.0");
        std::fs::create_dir_all(&broken).unwrap();

        let value = format!(
            "{}{}{}{}{}{}{}",
            base.join("tools").display(),
            style().separator,
            ninja.display(),
            style().separator,
            probe_rs.display(),
            style().separator,
            zig_dir.display()
        );
        assert_eq!(zig_dir_in_path(&value), Some(zig_dir.clone()));
        // A verbatim entry (written by an older fzv build) still resolves.
        assert_eq!(
            zig_dir_in_path(&format!(r"\\?\{}", zig_dir.display())),
            Some(zig_dir)
        );
        assert_eq!(
            zig_dir_in_path(&format!(
                "{}{}",
                base.join("tools").display(),
                style().separator
            )),
            None
        );
        assert_eq!(
            zig_dir_in_path(&format!("{}{}", broken.display(), style().separator)),
            None,
            "a directory without zig.exe is not an active version"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
