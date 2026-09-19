//! Windows activation: rewrite the user `PATH` in the registry.
//!
//! The Zig version directory itself becomes the `PATH` entry, so the active
//! version is whatever `PATH` points at. The rewrite is deliberately paranoid:
//! a failed read aborts instead of being treated as an empty `PATH`, the
//! registry value type is preserved, unrelated entries are never touched, and
//! the result is read back and verified.

use super::{Activation, zig_executable, zig_executable_in};
use crate::error::{Result, err};
use crate::path_util::{self, PathStyle};
use crate::version::Version;
use std::io;
use std::path::{Path, PathBuf};

const ENVIRONMENT_KEY: &str = "Environment";
const PATH_VALUE: &str = "Path";

pub fn active_zig_dir() -> Result<Option<PathBuf>> {
    Ok(crate::platform::zig_dir_in_path(&read_user_path()?))
}

pub fn activate(root: &Path, version: &Version) -> Result<Activation> {
    let directory = root.join(version.as_str());
    let executable = zig_executable_in(&directory);
    if !executable.is_file() {
        return Err(err!(
            "cannot activate Zig {version}; {} does not exist",
            executable.display()
        ));
    }
    let (saved, dropped) = rewrite(Some(&directory), root)?;
    let wanted_key = path_util::path_key(&directory.to_string_lossy(), style());
    if !saved
        .split(style().separator)
        .any(|entry| path_util::path_key(entry, style()) == wanted_key)
    {
        return Err(err!(
            "the user PATH was not saved with the Zig directory: {}",
            directory.display()
        ));
    }

    let mut notes: Vec<String> = dropped
        .into_iter()
        .map(|entry| format!("dropped stale PATH entry: {entry}"))
        .collect();
    notes.push("restart this terminal before running 'zig'".to_string());
    Ok(Activation {
        directory,
        notes,
    })
}

/// Removes every fzv-managed Zig directory from the user `PATH`.
pub fn deactivate(root: &Path) -> Result<()> {
    let (_, dropped) = rewrite(None, root)?;
    for entry in dropped {
        eprintln!("fzv: dropped stale PATH entry: {entry}");
    }
    Ok(())
}

fn style() -> PathStyle {
    PathStyle::windows()
}

/// Rewrites the user `PATH` for `root`, returning the saved value and the
/// entries that were dropped.
fn rewrite(wanted: Option<&Path>, root: &Path) -> Result<(String, Vec<String>)> {
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
    let new_path = path_util::rewrite_path(
        &old_path,
        root,
        wanted,
        style(),
        zig_executable(),
    );
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
    use winreg::{RegKey, enums::{HKEY_CURRENT_USER, KEY_READ}};

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
