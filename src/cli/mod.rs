//! The command line front end.
//!
//! This layer turns argv into calls on the layers below and prints what happened:
//! the domain modules never write to the terminal on purpose (only diagnostics
//! and progress do).

pub mod args;
pub mod commands;
pub mod prompt;

use crate::error::{Result, err};
use crate::log::detail;
use crate::path_util;
use crate::platform;
use crate::version::Version;
use args::Options;
use std::path::{Path, PathBuf};

/// Shown when the versions directory cannot be derived.
pub const NO_ROOT_HINT: &str = "no versions directory is known: pass one with '--path DIR' (for example 'fzv get dev --path D:/zig'), or activate a version with 'fzv use <version> --path DIR'";

/// The only place fzv keeps state is `PATH`: whichever directory it points at is
/// the active Zig version.
pub const NO_ACTIVE_HINT: &str =
    "PATH has no Zig version installed by fzv; run 'fzv use <version>' first";

/// Runs fzv and returns the process exit code.
pub fn run() -> i32 {
    // An update could not delete the binary it replaced while it was running;
    // this is where that leftover goes.
    if let Ok(current) = std::env::current_exe() {
        crate::update::clean_up_leftovers(&current);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

/// Runs the `zig`/`zls` a shim was asked for, returning the child's exit code.
///
/// A shim installed by fzv knows which versions directory it belongs to: from
/// its own location inside `<root>\.fzv\bin`, or - for the copies that sit next
/// to the launcher executable - from the shim entry in the user `PATH`. A
/// hand-made copy of fzv under one of those names falls back to resolving the
/// selection from `PATH`.
pub fn run_active_tool(tool: &str) -> Result<i32> {
    let executable = resolve_active_tool(tool)?;
    let status = std::process::Command::new(&executable)
        .args(std::env::args().skip(1))
        .status()
        .map_err(|error| err!("failed to launch {tool}: {error}"))?;
    Ok(platform::exit_code(&status))
}

/// The executable a `zig`/`zls` invocation has to end up in.
fn resolve_active_tool(tool: &str) -> Result<PathBuf> {
    if let Ok(shim) = std::env::current_exe()
        && (crate::shim::root_of_shim(&shim).is_some() || platform::shim_root_in_path()?.is_some())
    {
        return crate::shim::target_for_shim(tool, &shim);
    }
    active_tool_from_path(tool)
}

/// The executable to run for a `zig`/`zls` copy that is not one of fzv's shims:
/// it follows whatever `PATH` currently selects.
fn active_tool_from_path(tool: &str) -> Result<PathBuf> {
    let active = platform::active_zig_dir()?.ok_or_else(|| err!("{NO_ACTIVE_HINT}"))?;
    let root = active
        .parent()
        .ok_or_else(|| err!("{} has no parent directory", active.display()))?
        .to_path_buf();
    let executable = if tool == "zig" {
        platform::zig_executable_in(&active)
    } else {
        crate::install::ensure_zls(&root)?
    };
    if !executable.is_file() {
        return Err(err!(
            "{} does not exist; run 'fzv use' to (re)install a version",
            executable.display()
        ));
    }
    Ok(executable)
}

fn dispatch(args: &[String]) -> Result<()> {
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => ("h", &[][..]),
    };
    match command {
        "h" | "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "v" | "-v" | "--version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "ls" => commands::ls::run(&Options::parse(rest)?),
        "lls" => commands::lls::run(&Options::parse(rest)?),
        "path" => commands::path::run(&Options::parse(rest)?),
        "get" => commands::get::run(&Options::parse(rest)?),
        "rm" => commands::rm::run(&Options::parse(rest)?),
        "use" => commands::use_version::run(&Options::parse(rest)?),
        "update" | "upgrade" => commands::update::run(&Options::parse(rest)?),
        command => Err(err!(
            "unknown fzv command '{command}'; run 'fzv h' for help"
        )),
    }
}

fn print_help() {
    println!(
        "Usage: fzv <command> [options]

Manage Zig versions that are switched by a single entry in your PATH.

  ls [--path DIR]              list available Zig versions
  lls [--path DIR]             list versions installed in the versions directory
  get [VERSION...] [--path DIR] [--print-path]  download versions (choose when omitted)
  rm [VERSION...] [--yes] [--path DIR]  remove installed versions
  use [VERSION] [--path DIR] [--print-path]  activate a version
  path                         show the versions directory and the active version
  update [--force]             replace fzv with the newest GitHub release
  v                            print fzv version
  h                            show this help

'--force' installs the newest release even when it is the version already
running, which is how a broken installation is repaired.

Selectors:
  dev, master, latest   the newest development snapshot (published under /builds)
  stable                the newest stable release
  0.16.0                an exact version

Several selectors can be given at once, separated by spaces or commas:
  fzv get dev stable --path D:\\zig
  fzv get dev,stable --path D:\\zig

How it works:
  * fzv keeps copies of itself named 'zig.exe' and 'zls.exe' in
    '<versions>\\.fzv\\bin', which is the fzv entry in your user PATH, and the
    shims follow the version recorded in '<versions>\\.fzv\\active'. The same two
    copies are also written next to this executable (a directory your PATH
    already reaches), so 'fzv get' and 'fzv use' work in the terminal you are in
    right away: every terminal, IDE and build tool uses the new version on its
    next 'zig' invocation, and PATH is never touched again after the first time.
  * Community mirrors are measured and then used in order of throughput. A mirror
    that starts refusing requests - '429 Too Many Requests' is common on a public
    service - is dropped at once and the transfer continues from the next one,
    which matters more than which mirror was fastest.
  * The versions directory is taken from '--path DIR' when given, otherwise
    from the fzv entry already in PATH.
  * With '--path DIR', 'get' installs there and then activates the newest of
    the versions it installed, so later commands need no path.
  * Everything fzv keeps for a versions directory lives in '<versions>\\.fzv'
    (its shims, index cache and install locks); the only files it writes
    elsewhere are the two shim copies next to this executable.
  * If this executable sits somewhere PATH does not reach (or cannot be written
    to), the shims next to it are not possible and the terminal you are in has to
    pick up the PATH entry once - restart it, or apply what '--print-path' prints
    (status goes to stderr):

      PowerShell   $env:PATH = (fzv use 0.16.0 --print-path)
      bash, zsh    export PATH=\"$(fzv use 0.16.0 --print-path)\"
      cmd          for /f \"delims=\" %p in ('fzv use 0.16.0 --print-path') do set \"PATH=%p\"
      nushell      $env.PATH = (fzv use 0.16.0 --print-path | split row (char esep))

Paths: both \\ and / are accepted. Quote the value (\"D:\\PL_Collections\\zig\")
when your shell would otherwise strip the backslashes.

Environment:
  FZV_MIRROR                 download from a single mirror (for example https://example.org/zig)
  FZV_MIRRORS                mirrors to try, separated by commas (replaces the built-in list)
  FZV_NO_MIRRORS             always download from ziglang.org
  FZV_DOWNLOAD_JOBS          number of parallel download segments (default 8)
  FZV_REFRESH_INDEX          ignore the cached download index
  FZV_VERBOSE                also print mirror choice, checksums and unpacking
  FZV_REPO                   update from another repository (default ref42/fzv)
  FZV_RELEASES_URL           update from a GitHub Enterprise or internal server"
    );
}

/// The versions directory: `--path` when given, otherwise derived from `PATH`.
pub fn resolve_root(explicit: Option<&Path>) -> Result<PathBuf> {
    resolve_root_opt(explicit).ok_or_else(|| err!("{NO_ROOT_HINT}"))
}

/// Like [`resolve_root`], but `None` instead of an error: read-only commands can
/// simply report that they do not know where to look.
pub fn resolve_root_opt(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        // Existing directories are canonicalized so that `D:/zig` and `D:\zig`
        // (or a junction) cannot look like two different roots.
        return Some(
            path_util::canonical_path(path, platform::style())
                .unwrap_or_else(|_| path.to_path_buf()),
        );
    }
    // With shims installed, `PATH` names the versions directory directly.
    if let Some(root) = platform::shim_root_in_path().ok().flatten() {
        return Some(root);
    }
    let active = platform::active_zig_dir().ok().flatten()?;
    active
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// The directory of the active version, if any.
///
/// With shims installed the recorded version wins; otherwise the selection is
/// whatever the fzv directories in `PATH` point at.
pub fn active_version_dir(root: &Path) -> Option<PathBuf> {
    if crate::shim::is_installed(root)
        && let Ok(Some(version)) = crate::shim::active_version(root)
    {
        return Some(root.join(version.as_str()));
    }
    platform::active_zig_dir().ok().flatten()
}

/// Whether `directory` is the active version's directory.
pub fn is_active_version(root: &Path, directory: &Path) -> bool {
    let style = platform::style();
    let key = path_util::path_key(&directory.to_string_lossy(), style);
    active_version_dir(root)
        .is_some_and(|active| path_util::path_key(&active.to_string_lossy(), style) == key)
}

/// Prints the outcome of an activation.
///
/// This is a status report, so it goes to stderr: stdout is reserved for the
/// machine-readable `PATH` value `--print-path` produces. By default only the
/// active version is printed - which directories are in play, and whether `PATH`
/// changed, is [`crate::log`] detail.
pub fn report_activation(version: &Version, activation: &platform::Activation, apply_now: bool) {
    eprintln!("active Zig version: {version}");
    detail!(
        "fzv: zig directory: {}",
        path_util::display_path(&activation.zig_directory, platform::style())
    );
    match &activation.zls_directory {
        Some(directory) => detail!(
            "fzv: zls directory: {}",
            path_util::display_path(directory, platform::style())
        ),
        None => detail!("fzv: zls is not installed"),
    }
    for note in &activation.notes {
        detail!("fzv: {note}");
    }
    if apply_now {
        detail!("fzv: the calling shell applies the printed PATH");
    } else if activation.immediate {
        detail!("fzv: zig is already reachable in this terminal");
    } else {
        report_session_pickup(&activation.zig_directory);
    }
}

/// Reminds the user that the terminal they are in keeps the `PATH` it started
/// with: a process cannot change its parent's environment, so a brand new entry
/// has to be picked up (or the terminal restarted).
///
/// Only shown when fzv could not put the shims somewhere this terminal already
/// looks (a read-only or off-`PATH` launcher directory), and only once per
/// versions directory: repeating it on every later command would be noise.
fn report_session_pickup(zig_directory: &Path) {
    let Some(root) = zig_directory.parent() else {
        return;
    };
    if !crate::store::session_hint_once(root) {
        detail!("fzv: this terminal still predates the PATH entry");
        return;
    }
    eprintln!(
        "hint: restart this terminal to use zig in it (or apply the '--print-path' \
         one-liner from 'fzv h')"
    );
}

/// Prints the `PATH` value the calling shell should adopt.
///
/// See [`platform::session_path`]: a process cannot change its parent's
/// environment, so `fzv use` hands the value to the shell instead.
pub fn report_session_path(root: &Path) {
    println!("{}", platform::session_path(root));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_reports_unknown_commands() {
        let error = dispatch(&["nope".to_string()]).unwrap_err();
        assert!(error.to_string().contains("unknown fzv command"), "{error}");
    }

    #[test]
    fn explicit_paths_win_over_path_lookup() {
        let root = std::env::temp_dir().join(format!("fzv-cli-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(resolve_root(Some(&root)).unwrap(), root);
        assert_eq!(resolve_root_opt(Some(&root)), Some(root.clone()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
