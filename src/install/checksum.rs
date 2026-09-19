//! Verifying downloads.
//!
//! Mirrors are third-party servers, so every Zig archive is checked against the
//! SHA-256 the download index publishes. An archive is accepted, and kept on
//! disk, only when the bytes match.

use crate::error::{Result, err};
use crate::log::detail;
use std::fs;
use std::path::Path;

/// Checks a downloaded file against the checksum published in the download
/// index. Mirrors are third-party servers, so this is what makes them usable.
pub fn verify_sha256(path: &Path, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        detail!(
            "fzv: no published checksum for {}; skipping verification",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
        return Ok(());
    };
    let actual = sha256_file(path)?;
    if actual != expected {
        return Err(err!(
            "checksum mismatch for {}: expected {expected}, got {actual}",
            path.display()
        ));
    }
    detail!("fzv: checksum verified ({})", &actual[..16]);
    Ok(())
}

/// The SHA-256 of a file, in lower-case hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(crate::error::Error::from)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(crate::error::Error::from)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-checksum-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn verifies_published_checksums() {
        let root = temp_dir("sha");
        let archive = root.join("archive.bin");
        fs::write(&archive, b"abc").unwrap();
        assert_eq!(
            sha256_file(&archive).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex(&[0x00, 0xff, 0x10]), "00ff10");
        assert!(
            verify_sha256(
                &archive,
                Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            )
            .is_ok()
        );
        assert!(verify_sha256(&archive, Some("deadbeef")).is_err());
        // Without a published checksum the download is accepted as-is.
        assert!(verify_sha256(&archive, None).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
