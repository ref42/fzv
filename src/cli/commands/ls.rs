//! `fzv ls` — every published Zig version.

use super::no_positionals;
use crate::cli::args::Options;
use crate::error::Result;
use crate::index::Index;

pub fn run(options: &Options) -> Result<()> {
    no_positionals(options)?;
    let root = crate::cli::resolve_root_opt(options.path.as_deref());
    let index = Index::load(root.as_deref())?;
    for version in index.available() {
        println!("{version}\t{}", version.channel());
    }
    // What the selectors mean right now. On stderr, so the list above stays
    // convenient to parse.
    for selector in ["dev", "stable"] {
        if let Ok(version) = index.resolve(selector) {
            eprintln!("fzv: {selector} is currently {version}");
        }
    }
    Ok(())
}
