//! Zig version numbers as a type.
//!
//! Version strings double as directory names, download keys and selector
//! results, so parsing and ordering them is a domain concern rather than a
//! string utility: a [`Version`] can only be built by [`Version::parse`], and it
//! orders the way Zig itself does (a release outranks its own pre-releases, and
//! `dev.999` sorts below `dev.1000`).

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version(String);

impl Version {
    /// Parses `0.14.1` or `0.17.0-dev.2228+955228b68`.
    ///
    /// The result is also used as a directory name, so anything containing a
    /// separator is rejected.
    pub fn parse(text: &str) -> Option<Version> {
        let (core, suffix) = text.split_once('-').unwrap_or((text, ""));
        let mut parts = core.split('.');
        if ![parts.next()?, parts.next()?, parts.next()?]
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        if parts.next().is_some() {
            return None;
        }
        if !suffix.is_empty()
            && !suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".+_-".contains(c))
        {
            return None;
        }
        Some(Version(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is a pre-release rather than a finished release.
    ///
    /// Used to keep `stable` from resolving to something that is not a release.
    pub fn is_prerelease(&self) -> bool {
        self.0.contains('-')
    }

    /// The channel fzv reports for this version, matching how Zig publishes
    /// builds: only `-dev.` snapshots and `-rc.` candidates are "dev".
    pub fn channel(&self) -> &'static str {
        if self.0.contains("-dev.") || self.0.contains("-rc.") {
            "dev"
        } else {
            "stable"
        }
    }

    fn core_and_suffix(&self) -> (&str, &str) {
        self.0.split_once('-').unwrap_or((&self.0, ""))
    }

    /// Zig's own ordering, ignoring the tie-break that [`Ord`] adds.
    fn semver_cmp(&self, other: &Version) -> Ordering {
        let (left_core, left_suffix) = self.core_and_suffix();
        let (right_core, right_suffix) = other.core_and_suffix();
        let mut left = left_core.split('.');
        let mut right = right_core.split('.');
        loop {
            match (left.next(), right.next()) {
                (Some(a), Some(b)) => {
                    let order = numeric(a).cmp(&numeric(b));
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                (None, None) => break,
                (Some(_), None) => return Ordering::Greater,
                (None, Some(_)) => return Ordering::Less,
            }
        }
        match (left_suffix.is_empty(), right_suffix.is_empty()) {
            (true, true) => Ordering::Equal,
            // A release outranks its own pre-releases.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => compare_suffix(left_suffix, right_suffix),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Newest last. Versions that compare equal by Zig's rules (same number, only a
/// build hash apart) stay deterministic through the string comparison.
impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        self.semver_cmp(other).then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Sorts newest first.
pub fn sort_desc(versions: &mut [Version]) {
    versions.sort_by(|left, right| right.cmp(left));
}

fn numeric(text: &str) -> u64 {
    text.parse().unwrap_or(0)
}

/// Compares pre-release suffixes such as `dev.3028+abc` segment by segment.
fn compare_suffix(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) => {
                let order = compare_segment(a, b);
                if order != Ordering::Equal {
                    return order;
                }
            }
            (None, None) => return Ordering::Equal,
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
        }
    }
}

fn compare_segment(left: &str, right: &str) -> Ordering {
    if let (Ok(a), Ok(b)) = (left.parse::<u64>(), right.parse::<u64>()) {
        return a.cmp(&b);
    }
    let (left_number, left_rest) = split_leading_number(left);
    let (right_number, right_rest) = split_leading_number(right);
    left_number
        .cmp(&right_number)
        .then_with(|| left_rest.cmp(right_rest))
}

fn split_leading_number(text: &str) -> (u64, &str) {
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (numeric(&text[..digits]), &text[digits..])
}

#[cfg(test)]
mod tests {
    use super::{Version, sort_desc};
    use std::cmp::Ordering;

    fn version(text: &str) -> Version {
        Version::parse(text).expect(text)
    }

    #[test]
    fn parses_releases_and_dev_builds() {
        assert_eq!(version("0.13.0").as_str(), "0.13.0");
        assert_eq!(
            version("0.14.0-dev.3028+abc").as_str(),
            "0.14.0-dev.3028+abc"
        );
        assert!(Version::parse("0.13").is_none());
        assert!(Version::parse("zig-0.13.0").is_none());
        assert!(Version::parse("dev").is_none());
        assert!(Version::parse("0.14.0/..").is_none());
        assert!(Version::parse(r"C:\Windows").is_none());
        assert!(Version::parse("").is_none());
    }

    #[test]
    fn classifies_channels() {
        assert_eq!(version("0.14.1").channel(), "stable");
        assert!(!version("0.14.1").is_prerelease());
        assert_eq!(version("0.17.0-dev.2228+955228b68").channel(), "dev");
        assert_eq!(version("0.15.0-rc.1").channel(), "dev");
        assert!(version("0.15.0-rc.1").is_prerelease());
    }

    #[test]
    fn orders_versions_numerically() {
        assert_eq!(version("0.14.1").cmp(&version("0.14.1")), Ordering::Equal);
        assert_eq!(version("0.9.1").cmp(&version("0.14.1")), Ordering::Less);
        assert_eq!(
            version("0.14.10").cmp(&version("0.14.9")),
            Ordering::Greater
        );
        assert_eq!(
            version("0.14.0-dev.999").cmp(&version("0.14.0-dev.1000")),
            Ordering::Less
        );
        assert_eq!(
            version("0.14.0-dev.100").cmp(&version("0.14.0-dev.99")),
            Ordering::Greater
        );
        assert_eq!(
            version("0.14.0-rc.1").cmp(&version("0.14.0-rc.2")),
            Ordering::Less
        );
        // A release outranks its own pre-releases.
        assert_eq!(
            version("0.14.0-dev.9999").cmp(&version("0.14.0")),
            Ordering::Less
        );
        assert_eq!(
            version("0.14.0").cmp(&version("0.14.0-rc.1")),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_newest_first_and_is_deterministic() {
        let mut versions: Vec<Version> = [
            "0.13.0",
            "0.17.0-dev.2228+955228b68",
            "0.15.0",
            "0.14.0-dev.999",
            "0.14.0-dev.1000",
            "0.15.0-rc.1",
        ]
        .iter()
        .map(|text| version(text))
        .collect();
        sort_desc(&mut versions);
        let names: Vec<&str> = versions.iter().map(Version::as_str).collect();
        assert_eq!(
            names,
            [
                "0.17.0-dev.2228+955228b68",
                "0.15.0",
                "0.15.0-rc.1",
                "0.14.0-dev.1000",
                "0.14.0-dev.999",
                "0.13.0",
            ]
        );

        // Two builds that differ only by hash keep a stable relative order.
        let mut hashes: Vec<Version> = ["0.17.0-dev.1+bbb", "0.17.0-dev.1+aaa"]
            .iter()
            .map(|text| version(text))
            .collect();
        sort_desc(&mut hashes);
        assert_eq!(hashes[0].as_str(), "0.17.0-dev.1+bbb");
    }
}
