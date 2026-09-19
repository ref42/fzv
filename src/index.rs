//! The Zig download index.
//!
//! `https://ziglang.org/download/index.json` lists every published build, the
//! exact archive URL for each platform and a SHA-256 checksum. fzv uses it for
//! three things: listing available versions, resolving the `latest`/`stable`
//! selectors, and getting a verifiable download for a version.
//!
//! The parsed copy is cached in `<versions>/.fzv/download-index.json` for a few
//! hours; a stale copy is still used when the network is unavailable. Without a
//! known versions directory the index is fetched through a scratch file in the
//! system temp directory and not kept, so fzv still writes nothing permanent
//! outside the versions directory.

use crate::error::{Result, err};
use crate::json;
use crate::store;
use crate::version::{Version, sort_desc};
use std::path::Path;
use std::time::Duration;

pub const INDEX_URL: &str = "https://ziglang.org/download/index.json";

/// How long a cached index is trusted before it is fetched again.
const INDEX_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// A published archive for one version on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub url: String,
    pub file_name: String,
    /// Published SHA-256, when the index describes this build.
    pub sha256: Option<String>,
}

/// The parsed download index.
pub struct Index {
    document: json::Value,
}

impl Index {
    /// Loads the index, refreshing the cached copy when it is missing or old.
    pub fn load(root: Option<&Path>) -> Result<Index> {
        let text = match root {
            Some(root) => Self::cached_text(root)?,
            None => Self::fetch_to_memory()?,
        };
        let document = json::parse(&text)
            .map_err(|error| err!("unable to parse the download index: {error}"))?;
        Ok(Index { document })
    }

    fn cached_text(root: &Path) -> Result<String> {
        let path = store::index_cache(root)?;
        let cached = std::fs::read_to_string(&path).ok();
        let stale = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_none_or(|age| age > INDEX_MAX_AGE);
        if cached.is_none() || stale || std::env::var_os("FZV_REFRESH_INDEX").is_some() {
            match crate::download::download_once(INDEX_URL, &path) {
                Ok(()) => return std::fs::read_to_string(&path).map_err(crate::error::Error::from),
                Err(error) => match cached {
                    Some(text) => {
                        eprintln!("fzv: using the cached download index ({error})");
                        return Ok(text);
                    }
                    None => return Err(error),
                },
            }
        }
        Ok(cached.unwrap_or_default())
    }

    /// Fetches the index without keeping it (no versions directory is known yet).
    fn fetch_to_memory() -> Result<String> {
        let scratch = std::env::temp_dir().join(format!("fzv-index-{}.json", std::process::id()));
        let result = crate::download::download_once(INDEX_URL, &scratch)
            .and_then(|()| std::fs::read_to_string(&scratch).map_err(crate::error::Error::from));
        let _ = std::fs::remove_file(&scratch);
        result
    }

    /// Every published version, newest first, including the master snapshot
    /// (which the index only names inside its `master` entry).
    pub fn available(&self) -> Vec<Version> {
        let mut versions: Vec<Version> = self
            .document
            .keys()
            .filter_map(Version::parse)
            .collect();
        if let Some(master) = self.master_version() {
            versions.push(master);
        }
        versions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        versions.dedup();
        sort_desc(&mut versions);
        versions
    }

    /// Resolves `latest`, `stable`, `master`, `dev` or an exact version.
    pub fn resolve(&self, selector: &str) -> Result<Version> {
        match selector {
            "latest" | "master" | "dev" => self.master_version().ok_or_else(|| {
                err!("the download index has no valid master version")
            }),
            "stable" => self.stable_version(),
            value => Version::parse(value)
                .ok_or_else(|| err!("invalid Zig version '{value}'")),
        }
    }

    fn master_version(&self) -> Option<Version> {
        self.document
            .get("master")
            .and_then(|entry| entry.string_at("version"))
            .and_then(Version::parse)
    }

    fn stable_version(&self) -> Result<Version> {
        self.document
            .keys()
            .filter_map(Version::parse)
            .filter(|version| !version.is_prerelease())
            .max()
            .ok_or_else(|| err!("the download index contains no stable Zig releases"))
    }

    /// The archive the index publishes for `version` on this platform.
    pub fn archive(&self, version: &Version) -> Result<Option<Archive>> {
        Ok(self
            .find_archive(version.as_str(), platform_key()?)
            .map(|(url, file_name, sha256)| Archive {
                url,
                file_name,
                sha256,
            }))
    }

    /// Reads `version`/`platform` out of the document.
    fn find_archive(
        &self,
        version: &str,
        platform: &str,
    ) -> Option<(String, String, Option<String>)> {
        let entry = self.document.get(version)?.get(platform)?;
        let url = entry.string_at("tarball")?;
        let file_name = url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())?
            .to_string();
        Some((
            url.to_string(),
            file_name,
            entry.string_at("shasum").map(str::to_ascii_lowercase),
        ))
    }
}

/// Resolves a selector, loading the index only when it is needed.
pub fn resolve_selector(selector: &str, root: Option<&Path>) -> Result<Version> {
    Index::load(root)?.resolve(selector)
}

/// Everything needed to fetch one archive, preferring the index because it also
/// carries the checksum.
pub fn download_info(version: &Version, root: &Path) -> Result<Archive> {
    if version.as_str().ends_with("-mach") {
        return Err(err!(
            "mach versions require the Mach download index; this build does not have a configured mirror"
        ));
    }
    let platform = platform_key()?;
    match Index::load(Some(root)) {
        Ok(index) => {
            if let Some(archive) = index.archive(version)? {
                return Ok(archive);
            }
            eprintln!(
                "fzv: the download index does not describe Zig {version} for {platform}; deriving the archive URL (no checksum available)"
            );
        }
        Err(error) => {
            eprintln!(
                "fzv: unable to read the download index ({error}); deriving the archive URL (no checksum available)"
            );
        }
    }
    let (url, file_name) = constructed_archive(version, platform)?;
    Ok(Archive {
        url,
        file_name,
        sha256: None,
    })
}

/// The `{arch}-{platform}` key the index uses for this machine.
pub fn platform_key() -> Result<&'static str> {
    if cfg!(target_arch = "x86_64") {
        Ok("x86_64-windows")
    } else if cfg!(target_arch = "aarch64") {
        Ok("aarch64-windows")
    } else {
        Err(err!("unsupported CPU architecture for Zig"))
    }
}

/// The archive URL for a version the index does not describe, derived from Zig's
/// published naming conventions. Windows builds are always `.zip`.
pub fn constructed_archive(version: &Version, platform: &str) -> Result<(String, String)> {
    let arch = platform
        .split_once('-')
        .map(|(arch, _)| arch)
        .ok_or_else(|| err!("invalid platform key '{platform}'"))?;
    let text = version.as_str();
    // Development snapshots are published in /builds, not under the versioned
    // release directories.
    if text.contains("-dev.") || text.contains("-rc.") {
        let archive = format!("zig-{arch}-windows-{text}.zip");
        return Ok((format!("https://ziglang.org/builds/{archive}"), archive));
    }
    // Zig changed release archive naming from OS-ARCH to ARCH-OS in 0.14.1.
    let layout = if *version < Version::parse("0.14.1").expect("valid version") {
        format!("windows-{arch}")
    } else {
        format!("{arch}-windows")
    };
    let archive = format!("zig-{layout}-{text}.zip");
    Ok((
        format!("https://ziglang.org/download/{text}/{archive}"),
        archive,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(document: &str) -> Index {
        Index {
            document: json::parse(document).unwrap(),
        }
    }

    const DOCUMENT: &str = r#"{
        "master": {"version": "0.17.0-dev.2228+955228b68"},
        "0.13.0": {"x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.13.0/zig-windows-x86_64-0.13.0.zip",
            "shasum": "D859994725EF9402381E557C60BB57497215682E355204D754EE3DF75EE3C158"}},
        "0.14.1": {"x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.14.1/zig-x86_64-windows-0.14.1.zip",
            "shasum": "554f5378228923ffd558eac35e21af020c73789d87afeabf4bfd16f2e6feed2c"}},
        "0.15.0-rc.1": {"x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.15.0-rc.1/zig-x86_64-windows-0.15.0-rc.1.zip"}}
    }"#;

    #[test]
    fn lists_available_versions_newest_first() {
        let names: Vec<String> = index(DOCUMENT)
            .available()
            .into_iter()
            .map(|version| version.as_str().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "0.17.0-dev.2228+955228b68",
                "0.15.0-rc.1",
                "0.14.1",
                "0.13.0"
            ]
        );
    }

    #[test]
    fn resolves_selectors() {
        let index = index(DOCUMENT);
        assert_eq!(
            index.resolve("latest").unwrap().as_str(),
            "0.17.0-dev.2228+955228b68"
        );
        assert_eq!(index.resolve("master").unwrap().as_str(), "0.17.0-dev.2228+955228b68");
        assert_eq!(index.resolve("stable").unwrap().as_str(), "0.14.1");
        assert_eq!(index.resolve("0.13.0").unwrap().as_str(), "0.13.0");
        assert!(index.resolve("nope").is_err());
        assert!(index.resolve("0.13").is_err());
    }

    #[test]
    fn reads_archives_and_checksums() {
        let index = index(DOCUMENT);
        let archive = index
            .find_archive("0.13.0", "x86_64-windows")
            .expect("entry");
        assert_eq!(archive.1, "zig-windows-x86_64-0.13.0.zip");
        assert_eq!(
            archive.2.as_deref(),
            Some("d859994725ef9402381e557c60bb57497215682e355204d754ee3df75ee3c158")
        );
        // A build without a published checksum is still usable.
        let archive = index
            .find_archive("0.15.0-rc.1", "x86_64-windows")
            .expect("entry");
        assert_eq!(archive.2, None);
        // Unknown version or platform.
        assert!(index.find_archive("0.14.1", "aarch64-windows").is_none());
        assert!(index.find_archive("0.99.0", "x86_64-windows").is_none());
    }

    #[test]
    fn derives_archives_for_versions_outside_the_index() {
        let (url, archive) =
            constructed_archive(&Version::parse("0.13.0").unwrap(), "x86_64-windows").unwrap();
        assert_eq!(archive, "zig-windows-x86_64-0.13.0.zip");
        assert_eq!(url, format!("https://ziglang.org/download/0.13.0/{archive}"));

        let (_, archive) =
            constructed_archive(&Version::parse("0.14.1").unwrap(), "x86_64-windows").unwrap();
        assert_eq!(archive, "zig-x86_64-windows-0.14.1.zip");

        let dev = Version::parse("0.17.0-dev.2228+955228b68").unwrap();
        let (url, archive) = constructed_archive(&dev, "x86_64-windows").unwrap();
        assert_eq!(archive, "zig-x86_64-windows-0.17.0-dev.2228+955228b68.zip");
        assert_eq!(url, format!("https://ziglang.org/builds/{archive}"));

        // The aarch64 layout is derived from the platform key.
        let (_, archive) =
            constructed_archive(&Version::parse("0.13.0").unwrap(), "aarch64-windows").unwrap();
        assert_eq!(archive, "zig-windows-aarch64-0.13.0.zip");
    }
}
