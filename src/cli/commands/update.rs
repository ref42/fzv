//! `fzv update` - replace this fzv with the newest GitHub release.
//!
//! The work is in [`crate::update`]; this layer only decides which executable to
//! replace, which versions directory (if any) should have its shims refreshed,
//! and how to phrase the outcome. `--force` re-installs the newest release even
//! when it is the version that is already running.

use crate::cli::args::Options;
use crate::cli::resolve_root_opt;
use crate::error::{Result, err};
use crate::log::detail;
use crate::update::{self, Outcome, Releases};

pub fn run(options: &Options) -> Result<()> {
    super::no_positionals(options)?;
    let root = resolve_root_opt(options.path.as_deref());
    let executable = std::env::current_exe()
        .map_err(|error| err!("unable to locate the fzv executable: {error}"))?;
    match update::update(
        &executable,
        &Releases::configured(),
        env!("CARGO_PKG_VERSION"),
        root.as_deref(),
        options.force,
    )? {
        Outcome::Current { version } => {
            eprintln!("fzv {version} is up to date");
            eprintln!("use 'fzv update --force' to install the release again");
        }
        Outcome::Updated { from, to, path } => {
            if from == to {
                eprintln!(
                    "reinstalled fzv {to} ({})",
                    crate::path_util::display_path(&path, crate::platform::style())
                );
            } else {
                eprintln!(
                    "updated fzv {from} to {to} ({})",
                    crate::path_util::display_path(&path, crate::platform::style())
                );
            }
            eprintln!("the next 'fzv' or 'zig' invocation uses it; no terminal needs restarting");
            detail!("fzv: shims refreshed");
        }
    }
    Ok(())
}
