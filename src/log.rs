//! What fzv prints, and what it keeps quiet about.
//!
//! By default fzv prints only what a user has to act on: how far a download is,
//! what was installed, which version is now active, and errors. The details that
//! help when something is wrong (which mirror was picked, which checksum
//! matched, where an archive was unpacked) are noise when it is not, so they need
//! `FZV_VERBOSE`.

use std::ffi::OsString;
use std::sync::OnceLock;

/// Prints a diagnostic line, but only when `FZV_VERBOSE` asked for detail.
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
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| requested(std::env::var_os("FZV_VERBOSE")))
}

/// `FZV_VERBOSE=0`, `=no`, `=off` and `=false` all mean "no detail, thanks".
fn requested(value: Option<OsString>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let value = value.to_string_lossy().trim().to_ascii_lowercase();
    !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
}

#[cfg(test)]
mod tests {
    use super::requested;

    #[test]
    fn only_an_explicit_request_is_verbose() {
        assert!(!requested(None));
        for value in ["", " ", "0", "false", "FALSE", "no", "off"] {
            assert!(
                !requested(Some(value.into())),
                "{value:?} should stay quiet"
            );
        }
        for value in ["1", "true", "yes", "on", "2"] {
            assert!(requested(Some(value.into())), "{value:?} should be verbose");
        }
    }
}
