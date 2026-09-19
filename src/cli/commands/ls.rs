//! `fzv ls` — every published Zig version.

use super::no_positionals;
use crate::cli::args::Options;
use crate::error::Result;
use crate::index::Index;

pub fn run(options: &Options) -> Result<()> {
    no_positionals(options)?;
    let root = crate::cli::resolve_root_opt(options.path.as_deref());
    for version in Index::load(root.as_deref())?.available() {
        println!("{version}\t{}", version.channel());
    }
    Ok(())
}
