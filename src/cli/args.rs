//! Command line parsing.
//!
//! fzv's commands share one option shape — positional version selectors plus
//! `--path DIR` (and `--yes` for `rm`) — so they are parsed in one place. The
//! path argument is validated here as well, because that is where the most
//! common user mistake can be diagnosed.

use crate::error::{Result, err};
use crate::path_util::{self, PathStyle};
use std::path::PathBuf;

/// Advice shown when an argument looks like the shell deleted its backslashes.
const SHELL_ESCAPED_PATH_HINT: &str = "\nBoth separators are supported, but fzv received exactly the text above: if you typed 'D:\\PL_Collections\\zig', your shell deleted the backslashes.\nQuote the value ('D:\\PL_Collections\\zig') or write it with forward slashes (D:/PL_Collections/zig).";

/// The options a command receives.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Version selectors, in the order they were given.
    pub positionals: Vec<String>,
    /// An explicit versions directory.
    pub path: Option<PathBuf>,
    /// `--yes`: skip a confirmation prompt.
    pub yes: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Options> {
        let mut options = Options::default();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--path" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| err!("--path requires a directory"))?;
                    options.path = Some(parse_directory_argument(value)?);
                }
                "--yes" | "-y" => options.yes = true,
                value if value.starts_with('-') => {
                    return Err(err!("unknown option '{value}'"));
                }
                value => options.positionals.push(value.to_string()),
            }
            index += 1;
        }
        Ok(options)
    }
}

/// Validates a directory typed by the user.
///
/// Backslashes and forward slashes are both accepted. A shell that treats a
/// backslash as an escape character strips it before the argument reaches fzv,
/// which silently turns `D:\zig` into the drive-relative `D:zig`; that is
/// reported with the fix rather than accepted as a different directory.
pub fn parse_directory_argument(value: &str) -> Result<PathBuf> {
    let style = PathStyle::windows();
    // Scripts and `Start-Process` can hand the quotes through literally.
    let text = path_util::unquote(value.trim());
    if text.is_empty() {
        return Err(err!("the directory argument is empty"));
    }
    if path_util::is_drive_relative(text, style) {
        return Err(err!(
            "'{text}' is a drive-relative path, not an absolute one.{SHELL_ESCAPED_PATH_HINT}"
        ));
    }
    if !path_util::is_absolute(text, style) {
        // A rooted but drive-less path ("\zig") is what a mangled UNC or drive
        // path usually collapses into, so offer the same explanation there.
        let hint = if text.starts_with(['\\', '/']) {
            SHELL_ESCAPED_PATH_HINT
        } else {
            ""
        };
        return Err(err!("'{text}' is not an absolute path.{hint}"));
    }
    Ok(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options_in_any_order() {
        let directory = if cfg!(windows) { r"D:\zig" } else { "/tmp/zig" };
        let args: Vec<String> = ["0.14.1", "--path", directory, "dev"]
            .iter()
            .map(|part| part.to_string())
            .collect();
        let options = Options::parse(&args).unwrap();
        assert_eq!(options.positionals, ["0.14.1", "dev"]);
        assert_eq!(options.path, Some(PathBuf::from(directory)));
        assert!(!options.yes);

        let args: Vec<String> = ["--yes"].iter().map(|part| part.to_string()).collect();
        assert!(Options::parse(&args).unwrap().yes);
    }

    #[test]
    fn rejects_unknown_options_and_missing_values() {
        let args: Vec<String> = ["--nope"].iter().map(|part| part.to_string()).collect();
        assert!(Options::parse(&args).unwrap_err().to_string().contains("unknown option"));
        let args: Vec<String> = ["--path"].iter().map(|part| part.to_string()).collect();
        assert!(Options::parse(&args).unwrap_err().to_string().contains("requires a directory"));
    }

    #[test]
    fn explains_shell_mangled_paths() {
        let error = parse_directory_argument("D:PL_Collectionszig").unwrap_err();
        assert!(error.to_string().contains("drive-relative"), "{error}");
        assert!(error.to_string().contains("Quote the value"), "{error}");

        // A relative path gets a plain error, without the shell lecture.
        let error = parse_directory_argument("zig-versions").unwrap_err();
        assert!(error.to_string().contains("not an absolute path"), "{error}");
        assert!(!error.to_string().contains("Quote the value"), "{error}");
        assert!(parse_directory_argument("   ").is_err());
    }

    #[test]
    fn accepts_both_separators_and_wrapping_quotes() {
        for value in [
            r"D:\PL_Collections\zig",
            "D:/PL_Collections/zig",
            r"D:\PL_Collections/zig",
            r"D:\PL_Collections\zig\",
            r#"  "D:\PL_Collections\zig"  "#,
        ] {
            assert_eq!(
                parse_directory_argument(value).unwrap(),
                PathBuf::from(r"D:\PL_Collections\zig"),
                "input {value}"
            );
        }
        // A verbatim path pasted from a message works too.
        assert!(parse_directory_argument(r"\\?\D:\PL_Collections\zig").is_ok());
    }
}
