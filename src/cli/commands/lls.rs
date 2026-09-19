//! `fzv lls` — the versions installed in the versions directory.

use super::no_positionals;
use crate::cli;
use crate::cli::args::Options;
use crate::error::Result;
use crate::installed;

pub fn run(options: &Options) -> Result<()> {
    no_positionals(options)?;
    let Some(root) = cli::resolve_root_opt(options.path.as_deref()) else {
        println!("{}", cli::NO_ROOT_HINT);
        return Ok(());
    };
    let versions = installed::scan(&root)?;
    if versions.is_empty() {
        println!("no local Zig versions installed in {}", root.display());
        return Ok(());
    }
    for installed in versions {
        let mut markers = Vec::new();
        if cli::is_active_version(&root, &installed.directory(&root)) {
            markers.push("active");
        }
        if !installed.ready {
            markers.push("incomplete");
        }
        println!("{}\t{}", installed.name, markers.join(","));
    }
    Ok(())
}
