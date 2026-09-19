//! `fzv get` - download versions into the versions directory.
//!
//! With `--path DIR` the command also activates the newest version it installed:
//! passing a path is how a user says "work in this directory from now on", and
//! doing both at once avoids installing into one directory while `PATH` still
//! points at another.

use crate::cli;
use crate::cli::args::Options;
use crate::cli::prompt;
use crate::error::Result;
use crate::index::{self, Index};
use crate::install;
use crate::platform;
use crate::version::{Version, sort_desc};

pub fn run(options: &Options) -> Result<()> {
    let root = cli::resolve_root(options.path.as_deref())?;

    let mut selected: Vec<Version> = Vec::new();
    if options.positionals.is_empty() {
        let versions: Vec<String> = Index::load(Some(&root))?
            .available()
            .iter()
            .map(|version| version.as_str().to_string())
            .collect();
        for picked in prompt::choose("Get Zig versions", &versions, true)? {
            if let Some(version) = Version::parse(&picked)
                && !selected.contains(&version)
            {
                selected.push(version);
            }
        }
    } else {
        for selector in &options.positionals {
            let version = index::resolve_selector(selector, Some(&root))?;
            if !selected.contains(&version) {
                selected.push(version);
            }
        }
    }
    if selected.is_empty() {
        return Ok(());
    }
    sort_desc(&mut selected);

    install::ensure_zls(&root)?;
    for version in &selected {
        install::ensure_zig(&root, version)?;
        eprintln!("installed {version}");
    }

    if options.path.is_some() {
        let activation = platform::activate(&root, &selected[0])?;
        cli::report_activation(&selected[0], &activation, options.print_path);
        if options.print_path {
            cli::report_session_path(&root);
        }
    }
    Ok(())
}
