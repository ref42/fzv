//! `fzv rm` - delete installed versions.
//!
//! Removing the active version also removes fzv's `PATH` entry (or, on Unix, its
//! symlink), because it would otherwise point at a directory that no longer
//! exists.

use crate::cli;
use crate::cli::args::Options;
use crate::cli::prompt;
use crate::error::{Result, err};
use crate::installed;
use crate::platform;
use crate::version::Version;

pub fn run(options: &Options) -> Result<()> {
    let root = cli::resolve_root(options.path.as_deref())?;

    let names: Vec<String> = if options.positionals.is_empty() {
        let installed = installed::scan(&root)?;
        if installed.is_empty() {
            println!("no local Zig versions installed in {}", root.display());
            return Ok(());
        }
        let names: Vec<String> = installed
            .into_iter()
            .map(|installed| installed.name)
            .collect();
        let picked = prompt::choose("Remove local Zig versions", &names, true)?;
        if picked.is_empty() {
            return Ok(());
        }
        picked
    } else {
        options.positionals.clone()
    };

    // Names become directory names, so they are validated before anything is
    // deleted. "dev" is the directory older fzv builds used for snapshots.
    let names: Vec<String> = names
        .into_iter()
        .map(|name| {
            if name == "dev" || Version::parse(&name).is_some() {
                Ok(name)
            } else {
                Err(err!("invalid Zig version '{name}'"))
            }
        })
        .collect::<Result<_>>()?;

    if !options.yes && !prompt::confirm(&format!("Remove {} version(s)?", names.len()))? {
        return Ok(());
    }

    for name in names {
        let directory = root.join(&name);
        if !directory.is_dir() {
            return Err(err!(
                "Zig {name} is not installed in {}",
                root.display()
            ));
        }
        let was_active = cli::is_active_version(&directory);
        std::fs::remove_dir_all(&directory)
            .map_err(|error| err!("unable to remove {}: {error}", directory.display()))?;
        if was_active {
            platform::deactivate(&root)?;
            println!("removed {name} (it was active; run 'fzv use' to activate another one)");
        } else {
            println!("removed {name}");
        }
    }
    Ok(())
}
