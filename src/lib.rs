//! fzv - manage the Zig version your `PATH` points at (Windows).
//!
//! # Layers
//!
//! ```text
//!   main.rs            process entry point (exit code only)
//!     cli/             argument parsing, prompts, per-command behaviour
//!       commands/      one module per fzv command
//!     install/         downloading, verifying and unpacking a version
//!       archive.rs     .zip extraction with guard rails
//!       checksum.rs    SHA-256 verification of downloads
//!     index.rs         the ziglang.org download index and its cache
//!     installed.rs     what is installed in a versions directory
//!     layout.rs        finding and flattening an installed executable
//!     version.rs       the Version type (parsing and ordering)
//!     platform.rs      Windows integration: the user PATH in the registry
//!     path_util.rs     pure PATH rewriting rules
//!     download/        HTTP transport, mirrors, progress
//!     store.rs         where fzv's own files live
//!     json.rs          a small JSON reader
//!     error.rs         Error / Result
//! ```
//!
//! # The one invariant
//!
//! The active version *is* the Zig directory in `HKCU\Environment\Path`: `fzv use`
//! replaces that single entry with `<versions>\<version>`. Nothing else records
//! the selection, so fzv cannot drift out of sync with the shell, and every fzv
//! file lives inside the versions directory (`<versions>\.fzv`).

pub mod cli;
pub mod download;
pub mod error;
pub mod index;
pub mod install;
pub mod installed;
pub mod json;
pub mod layout;
pub mod path_util;
pub mod platform;
pub mod store;
pub mod version;

use std::path::{Path, PathBuf};

/// Process entry point: returns the exit code.
pub fn entry() -> i32 {
    let tool = invoked_tool();
    let result = match tool.as_deref() {
        Some(tool) => cli::run_active_tool(tool),
        None => return cli::run(),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

/// Detects a `zig`/`zls` shim (a copy of fzv under a different name).
///
/// `argv[0]` is checked first on purpose: `env::current_exe()` resolves symbolic
/// links, so a Unix `ln -s fzv ~/.local/bin/zig` would otherwise look exactly
/// like a plain `fzv` invocation.
fn invoked_tool() -> Option<String> {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::current_exe().ok())
        .find_map(|path| tool_name(&path))
}

fn tool_name(path: &Path) -> Option<String> {
    let name = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
    (name == "zig" || name == "zls").then_some(name)
}

#[cfg(test)]
mod tests {
    use super::tool_name;
    use std::path::Path;

    #[test]
    fn recognises_shim_names() {
        assert_eq!(tool_name(Path::new("/usr/bin/zig")).as_deref(), Some("zig"));
        assert_eq!(tool_name(Path::new("C:\\tools\\ZIG.EXE")).as_deref(), Some("zig"));
        assert_eq!(tool_name(Path::new("/usr/bin/zls")).as_deref(), Some("zls"));
        assert_eq!(tool_name(Path::new("/usr/bin/fzv")), None);
    }
}
