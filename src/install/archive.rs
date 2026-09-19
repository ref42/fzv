//! Unpacking downloaded archives.
//!
//! Zig and ZLS both publish `.zip` archives for Windows, so that is the only
//! format fzv has to handle. Extraction is defensive: entries that would escape
//! the destination are refused, and an entry/size budget stops an archive from
//! filling the disk.

use crate::error::{Error, Result, err};
use std::fs;
use std::io;
use std::path::Path;

pub fn extract_archive(archive: &Path, destination: &Path) -> Result<()> {
    eprintln!("fzv: extracting {}...", archive.display());
    fs::create_dir_all(destination).map_err(Error::from)?;
    let mut budget = ExtractBudget::default();
    extract_zip(archive, destination, &mut budget)
}

/// Extraction guard rails. The real Zig and ZLS archives stay far below these
/// limits, but a hostile archive cannot fill the disk.
#[derive(Default)]
struct ExtractBudget {
    entries: usize,
    bytes: u64,
}

impl ExtractBudget {
    const MAX_ENTRIES: usize = 200_000;
    const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

    fn take(&mut self, name: &str, size: u64) -> Result<()> {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(size);
        if self.entries > Self::MAX_ENTRIES || self.bytes > Self::MAX_BYTES {
            return Err(err!(
                "refusing to extract '{name}': the archive is unreasonably large"
            ));
        }
        Ok(())
    }
}

fn extract_zip(archive: &Path, destination: &Path, budget: &mut ExtractBudget) -> Result<()> {
    let file = fs::File::open(archive).map_err(Error::from)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| err!("unable to open ZIP archive: {error}"))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| err!("unable to read ZIP entry {index}: {error}"))?;
        // `enclosed_name` rejects entries that would escape the destination.
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| err!("unsafe path in ZIP archive: {}", entry.name()))?
            .to_path_buf();
        budget.take(entry.name(), entry.size())?;
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(Error::from)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(Error::from)?;
        }
        let mut file = fs::File::create(&output).map_err(Error::from)?;
        io::copy(&mut entry, &mut file).map_err(Error::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-archive-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn extracts_zip_archives() {
        let root = temp_dir("zip");
        let archive = root.join("archive.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("zig-x86_64-windows-0.0.1-test/zig.exe", options)
                .unwrap();
            zip.write_all(b"exe").unwrap();
            zip.start_file("zig-x86_64-windows-0.0.1-test/lib/std.bin", options)
                .unwrap();
            zip.write_all(b"lib").unwrap();
            let file = zip.finish().unwrap();
            file.sync_all().unwrap();
        }
        let out = root.join("out");
        extract_archive(&archive, &out).unwrap();
        let prefix = out.join("zig-x86_64-windows-0.0.1-test");
        assert_eq!(fs::read(prefix.join("zig.exe")).unwrap(), b"exe");
        assert_eq!(fs::read(prefix.join("lib").join("std.bin")).unwrap(), b"lib");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_an_entry_that_escapes_the_destination() {
        let root = temp_dir("escape");
        let archive = root.join("archive.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            // The writer is happy to store such a name; the reader must refuse
            // to extract it.
            zip.start_file("../escape.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"boom").unwrap();
            zip.finish().unwrap().sync_all().unwrap();
        }
        let out = root.join("out");
        let error = extract_archive(&archive, &out).unwrap_err();
        assert!(error.to_string().contains("unsafe path"), "{error}");
        assert!(!root.join("escape.exe").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_archives_that_exceed_the_extraction_budget() {
        let mut budget = ExtractBudget {
            entries: ExtractBudget::MAX_ENTRIES,
            bytes: 0,
        };
        assert!(budget.take("one-more", 0).is_err());
        let mut budget = ExtractBudget {
            entries: 0,
            bytes: ExtractBudget::MAX_BYTES,
        };
        assert!(budget.take("one-more", 1).is_err());
        let mut budget = ExtractBudget::default();
        assert!(budget.take("fine", 1024).is_ok());
    }
}
