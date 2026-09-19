//! What fzv prints, and what it keeps quiet about.
//!
//! By default fzv prints only what a user has to act on: how far a download is,
//! what was installed, which version is now active, and errors. The details that
//! help when something is wrong (which mirror was picked, which checksum
//! matched, where an archive was unpacked) are printed when `-verbose` asks for
//! them.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `-verbose` was given. The command line sets this once, before any
/// work starts, so the layers below can ask without it being threaded through
/// every call.
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Prints a diagnostic line, but only when `-verbose` asked for detail.
macro_rules! detail {
    ($($arg:tt)*) => {
        if $crate::log::verbose() {
            eprintln!($($arg)*);
        }
    };
}

pub(crate) use detail;

/// Whether diagnostic detail was asked for.
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Remembers that `-verbose` was given.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::{set_verbose, verbose};

    #[test]
    fn the_flag_is_what_turns_detail_on() {
        set_verbose(true);
        assert!(verbose());
        set_verbose(false);
        assert!(!verbose());
    }
}
