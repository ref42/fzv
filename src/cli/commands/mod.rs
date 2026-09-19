//! One module per fzv command.
//!
//! The read-only commands (`ls`, `lls`, `path`) only look at the versions
//! directory, the download index and `PATH`; the others (`get`, `rm`, `use`)
//! change what is installed or which version is active.

pub mod get;
pub mod lls;
pub mod ls;
pub mod path;
pub mod rm;
pub mod use_version;

use crate::cli::args::Options;
use crate::error::{Result, err};

/// Rejects positional arguments a command does not take.
fn no_positionals(options: &Options) -> Result<()> {
    match options.positionals.first() {
        Some(argument) => Err(err!("unexpected argument '{argument}'")),
        None => Ok(()),
    }
}
