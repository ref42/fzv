//! The crate error type.
//!
//! fzv reports failures as short human-readable messages: the CLI prints them
//! verbatim and exits non-zero, and everything below that layer only has to
//! decide *what* went wrong. A newtype over `String` keeps that ergonomic while
//! still giving one place to render, wrap and test errors.

use std::fmt;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error(error.to_string())
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error(message.to_string())
    }
}

/// The crate result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Builds an [`Error`] with `format!` syntax.
macro_rules! err {
    ($($arg:tt)*) => {
        $crate::error::Error::new(format!($($arg)*))
    };
}

pub(crate) use err;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn renders_messages() {
        assert_eq!(Error::new("boom").to_string(), "boom");
        assert_eq!(err!("{} {}", "a", 1).message(), "a 1");
        assert_eq!(Error::from(std::io::Error::other("x")).message(), "x");
    }
}
