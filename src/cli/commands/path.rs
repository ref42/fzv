//! `fzv path` — where fzv looks, and what is active.

use crate::cli;
use crate::cli::args::Options;
use crate::error::{Result, err};
use crate::installed;

pub fn run(options: &Options) -> Result<()> {
    if let Some(argument) = options.positionals.first() {
        // The versions directory is not stored anywhere, so there is nothing to
        // set: say how to choose one instead.
        return Err(err!(
            "'{argument}' is not used: the versions directory comes from '--path' or from PATH.\nTo switch to another one, activate a version from it: fzv use <version> --path DIR"
        ));
    }
    let Some(root) = cli::resolve_root_opt(options.path.as_deref()) else {
        println!("{}", cli::NO_ROOT_HINT);
        return Ok(());
    };
    println!("versions directory: {}", root.display());
    match cli::active_version_dir() {
        Some(directory) => println!("active version: {}", directory.display()),
        None => println!("active version: none (PATH has no fzv Zig directory)"),
    }
    let versions = installed::scan(&root)?;
    if versions.is_empty() {
        println!("no local Zig versions installed");
    } else {
        let names: Vec<&str> = versions
            .iter()
            .map(|installed| installed.name.as_str())
            .collect();
        println!("installed: {}", names.join(", "));
    }
    Ok(())
}
