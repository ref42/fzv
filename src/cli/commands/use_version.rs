//! `fzv use` - activate a version.
//!
//! Activation is a single edit of the user's `PATH` (or a single symlink on
//! Unix). No version is recorded anywhere else, so `PATH` cannot disagree with
//! fzv about what is active.

use crate::cli;
use crate::cli::args::{self, Options};
use crate::cli::prompt;
use crate::error::{Result, err};
use crate::index;
use crate::install;
use crate::installed;
use crate::log::detail;
use crate::platform;

pub fn run(options: &Options) -> Result<()> {
    if options.positionals.len() > 1 {
        return Err(err!("usage: fzv use [VERSION] [--path DIR]"));
    }
    let root = cli::resolve_root(options.path.as_deref())?;

    let version = match options.positionals.first() {
        Some(selector) => index::resolve_selector(selector, Some(&root))?,
        None => {
            let selectable = installed::selectable(&root)?;
            if selectable.is_empty() {
                println!(
                    "no usable Zig versions in {}; run 'fzv get <version> --path {}' first",
                    root.display(),
                    root.display()
                );
                return Ok(());
            }
            let names: Vec<String> = selectable
                .iter()
                .map(|version| version.as_str().to_string())
                .collect();
            let picked = prompt::choose("Use Zig version", &names, false)?;
            match picked.first().and_then(|name| {
                selectable
                    .into_iter()
                    .find(|version| version.as_str() == name)
            }) {
                Some(version) => version,
                None => return Ok(()),
            }
        }
    };

    let executable = install::ensure_zig(&root, &version, args::DEFAULT_JOBS)?;
    let zls = install::ensure_zls(&root, args::DEFAULT_JOBS)?;
    let activation = platform::activate(&root, &version)?;
    cli::report_activation(&version, &activation);
    detail!("fzv: zig executable: {}", executable.display());
    detail!("fzv: zls executable: {}", zls.display());
    Ok(())
}
