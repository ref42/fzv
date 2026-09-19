//! The command line front end.
//!
//! This layer turns argv into calls on the layers below and prints what happened:
//! the domain modules never write to the terminal on purpose (only diagnostics
//! and progress do).

pub mod args;
pub mod commands;
pub mod prompt;

use crate::error::{Result, err};
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
pub fn run_active_tool(tool: &str) -> Result<i32> {
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
    let status = std::process::Command::new(&executable)
        .args(std::env::args().skip(1))
        .status()
        .map_err(|error| err!("failed to launch {tool}: {error}"))?;
    Ok(platform::exit_code(&status))
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
        command => Err(err!("unknown fzv command '{command}'; run 'fzv h' for help")),
    }
}

fn print_help() {
    println!(
        "Usage: fzv <command> [options]

Manage Zig versions that are switched by a single entry in your PATH.

  ls [--path DIR]              list available Zig versions
  lls [--path DIR]             list versions installed in the versions directory
  get [VERSION...] [--path DIR]  download versions (choose when omitted)
  rm [VERSION...] [--yes] [--path DIR]  remove installed versions
  use [VERSION] [--path DIR]   activate a version by rewriting that PATH entry
  path                         show the versions directory and the active version
  v                            print fzv version
  h                            show this help

Selectors: latest, stable, master, or an exact version.

How it works:
  * 'fzv use <version>' replaces the fzv Zig directory in your user PATH with
    '<versions>\\<version>'; that PATH entry is the active version, so fzv keeps
    no other record of it.
  * The versions directory is taken from '--path DIR' when given, otherwise
    from the Zig directory already in PATH.
  * With '--path DIR', 'get' installs there and then activates the newest of
    the versions it installed, so later commands need no path.
  * fzv writes nothing outside the versions directory; its index cache and
    its install locks live in '<versions>\\.fzv'.

Paths: both \\ and / are accepted. Quote the value (\"D:\\PL_Collections\\zig\")
when your shell would otherwise strip the backslashes.

Environment:
  FZV_MIRROR                 download from a single mirror (for example https://example.org/zig)
  FZV_NO_MIRRORS             always download from ziglang.org
  FZV_DOWNLOAD_JOBS          number of parallel download segments (default 8)
  FZV_REFRESH_INDEX          ignore the cached download index"
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
            path_util::canonical_path(path, platform::style()).unwrap_or_else(|_| path.to_path_buf()),
        );
    }
    let active = platform::active_zig_dir().ok().flatten()?;
    active
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// The directory of the active version, if any.
pub fn active_version_dir() -> Option<PathBuf> {
    platform::active_zig_dir().ok().flatten()
}

/// Prints the outcome of an activation.
pub fn report_activation(version: &Version, activation: &platform::Activation) {
    println!("active Zig version: {version}");
    println!(
        "PATH directory: {}",
        path_util::display_path(&activation.directory, platform::style())
    );
    for note in &activation.notes {
        println!("{note}");
    }
}

/// Whether `directory` is the active version's directory.
pub fn is_active_version(directory: &Path) -> bool {
    let style = platform::style();
    let key = path_util::path_key(&directory.to_string_lossy(), style);
    active_version_dir()
        .is_some_and(|active| path_util::path_key(&active.to_string_lossy(), style) == key)
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
