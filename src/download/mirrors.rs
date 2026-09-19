//! Choosing where to download an archive from.
//!
//! Latency alone is a poor selector: a mirror can answer a tiny request quickly
//! and then deliver the archive at dial-up speed. Each candidate is therefore
//! measured for throughput on a 256 KiB sample, and the result is a *ranked list*
//! rather than one winner - a mirror that starts answering `429 Too Many
//! Requests` half way through is normal on a public service, and the answer is
//! to continue from the next mirror, not to fail the download.
//!
//! Measuring takes a moment, so the caller sees a spinner rather than a pause;
//! which mirrors were found and why one was dropped is [`crate::log`] detail,
//! except the switches themselves, which the user is told about.

use crate::log::detail;
use crate::progress::Spinner;
use reqwest::StatusCode;
use std::env;
use std::path::Path;
use std::sync::Mutex;

/// Community mirrors of the Zig download directory.
const ZIG_MIRRORS: &[&str] = &[
    "https://pkg.hexops.org/zig",
    "https://zigmirror.hryx.net/zig",
    "https://zig.linus.dev/zig",
    "https://zig.squirl.dev",
    "https://zig.mirror.mschae23.de/zig",
    "https://ziglang.freetls.fastly.net",
    "https://zig.tilok.dev",
    "https://zig-mirror.tsimnet.eu/zig",
    "https://zig.karearl.com/zig",
    "https://pkg.earth/zig",
    "https://fs.liujiacai.net/zigbuilds",
    "https://zigmirror.com",
    "https://zig.chainsafe.dev",
    "https://zig.savalione.com",
    "https://zig.bcr.ist",
    "https://zig.vortan.dev/zig",
];

/// One place an archive can be fetched from.
#[derive(Debug, Clone)]
pub(super) struct Source {
    pub url: String,
    /// Whether this source answers byte ranges, i.e. whether chunks can be
    /// fetched from it.
    pub ranges: bool,
    /// The archive length this source reported, when it reported one.
    pub total: Option<u64>,
}

impl Source {
    /// The host, for messages: `https://zig.example/zig/x.zip` -> `zig.example`.
    pub fn host(&self) -> String {
        self.url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or(&self.url)
            .to_string()
    }
}

/// What is known about one candidate.
#[derive(Debug, Clone, Copy, Default)]
struct Candidate {
    /// It failed for good: neither pass should ask it again.
    failed: bool,
    /// Whether it can serve byte ranges, once that has been established. The
    /// probes answer this for mirrors; anything else - the official URL - has to
    /// be asked with a HEAD the first time it is used for chunks.
    ranges: Option<bool>,
}

/// The sources to try, in order.
///
/// Both passes - chunks and one stream - walk this list from the start and skip
/// whatever has failed, so a source that one pass cannot use (one that does not
/// answer byte ranges) is still available to the other.
pub(super) struct Sources {
    entries: Vec<Source>,
    candidates: Mutex<Vec<Candidate>>,
}

impl Sources {
    pub(super) fn new(entries: Vec<Source>) -> Self {
        let candidates = entries
            .iter()
            .map(|source| Candidate {
                failed: false,
                ranges: source.ranges.then_some(true),
            })
            .collect();
        Sources {
            entries,
            candidates: Mutex::new(candidates),
        }
    }

    pub(super) fn entries(&self) -> &[Source] {
        &self.entries
    }

    /// Finds out whether `index` can serve chunks, asking it once with a HEAD
    /// when its probes never said. Returns `false` for a source that cannot, and
    /// for one that has already failed.
    pub(super) async fn can_serve_chunks(&self, client: &reqwest::Client, index: usize) -> bool {
        use reqwest::header::ACCEPT_RANGES;

        let known = lock(&self.candidates)[index];
        if known.failed {
            return false;
        }
        if let Some(ranges) = known.ranges {
            return ranges;
        }
        let source = &self.entries[index];
        let ranges = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.head(&source.url).send(),
        )
        .await
        {
            Ok(Ok(response)) if response.status().is_success() => response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("bytes")),
            _ => false,
        };
        lock(&self.candidates)[index].ranges = Some(ranges);
        if !ranges {
            detail!(
                "fzv: {} cannot serve chunks; leaving it for a streaming pass",
                source.host()
            );
        }
        ranges
    }

    /// Records that `index` is unusable, telling the user which source is tried
    /// next - a silent switch would look like a stall.
    pub(super) fn mark_failed(
        &self,
        index: usize,
        error: &str,
        progress: Option<&crate::progress::Progress>,
    ) {
        let (first, next) = {
            let mut state = lock(&self.candidates);
            let first = !state[index].failed;
            state[index].failed = true;
            let next = (index + 1..self.entries.len()).find(|candidate| !state[*candidate].failed);
            (first, next)
        };
        // Several connections notice the same unusable source at once; one line
        // is what the user needs.
        if !first {
            return;
        }
        let message = match next {
            Some(next) => format!(
                "fzv: {} failed ({error}); trying {}",
                self.entries[index].host(),
                self.entries[next].host()
            ),
            None => format!(
                "fzv: {} failed ({error}); no source left",
                self.entries[index].host()
            ),
        };
        match progress {
            Some(progress) => progress.suspend(|| eprintln!("{message}")),
            None => eprintln!("{message}"),
        }
    }

    /// Whether anything is still worth trying.
    pub(super) fn any_left(&self) -> bool {
        lock(&self.candidates).iter().any(|state| !state.failed)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a response means "this source is not usable right now" rather than
/// "this request failed".
///
/// Rate limiting, an auth wall, a missing file and a broken server are all
/// reasons to fetch from somewhere else; a network hiccup on one connection is
/// not (that request is simply retried).
pub(super) fn is_source_failure(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        401 | 403 | 404 | 409 | 410 | 423 | 429 | 451
    ) || status.is_server_error()
}

/// The base URLs to consider, best first, before probing.
///
/// `FZV_MIRRORS` replaces the built-in list entirely (comma or space separated),
/// `FZV_MIRROR` keeps the list to a single mirror.
pub(super) fn candidate_bases(list: Option<&str>, single: Option<&str>) -> Vec<String> {
    if let Some(single) = single {
        let single = single.trim();
        if !single.is_empty() {
            return vec![single.trim_end_matches('/').to_string()];
        }
    }
    match list {
        Some(list) if !list.trim().is_empty() => list
            .split([',', ' ', ';'])
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .map(|base| base.trim_end_matches('/').to_string())
            .collect(),
        _ => ZIG_MIRRORS.iter().map(|base| base.to_string()).collect(),
    }
}

/// The sources for `output`, best first, ending with `official_url`.
///
/// Mirrors are only used for Zig archives: the index and the ZLS release are not
/// mirrored.
pub(super) async fn select_download_sources(
    client: &reqwest::Client,
    official_url: &str,
    output: &Path,
) -> Sources {
    use reqwest::header::{CONTENT_RANGE, RANGE};
    use std::time::{Duration, Instant};
    use tokio::task::JoinSet;

    let official = Source {
        url: official_url.to_string(),
        ranges: false,
        total: None,
    };
    let Some(filename) = output.file_name().and_then(|name| name.to_str()) else {
        return Sources::new(vec![official]);
    };
    if !filename.starts_with("zig-")
        || !filename.ends_with(".zip")
        || env::var_os("FZV_NO_MIRRORS").is_some()
    {
        return Sources::new(vec![official]);
    }

    let bases = candidate_bases(
        env::var("FZV_MIRRORS").ok().as_deref(),
        env::var("FZV_MIRROR").ok().as_deref(),
    );
    let candidates: Vec<String> = bases
        .into_iter()
        .map(|base| format!("{base}/{filename}?source=fzv"))
        .collect();

    // Latency alone is a poor mirror selector: a mirror can answer a tiny
    // request quickly and then deliver the archive at dial-up speed. Measure
    // enough data to compare throughput instead.
    const SAMPLE_SIZE: u64 = 256 * 1024;
    // The probe is a pause the user should be able to see the reason for.
    let _spinner = Spinner::start("looking for a fast mirror");
    let mut probes = JoinSet::new();
    for candidate in candidates {
        let client = client.clone();
        probes.spawn(async move {
            let started = Instant::now();
            let probe = async {
                let mut response = client
                    .get(&candidate)
                    .header(RANGE, format!("bytes=0-{}", SAMPLE_SIZE - 1))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let supports_ranges = response.status() == StatusCode::PARTIAL_CONTENT;
                if !supports_ranges && !response.status().is_success() {
                    return Err(format!("HTTP {}", response.status()));
                }
                let total = if supports_ranges {
                    response
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.rsplit_once('/'))
                        .and_then(|(_, total)| total.parse::<u64>().ok())
                } else {
                    response.content_length()
                };
                let mut received = 0u64;
                while received < SAMPLE_SIZE {
                    let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? else {
                        break;
                    };
                    received += chunk.len() as u64;
                }
                if received < SAMPLE_SIZE {
                    return Err(format!("probe returned only {received} bytes"));
                }
                Ok((candidate, supports_ranges, total, started.elapsed()))
            };
            tokio::time::timeout(Duration::from_secs(8), probe)
                .await
                .map_err(|_| "probe timed out".to_string())?
        });
    }

    let mut measured = Vec::new();
    while let Some(result) = probes.join_next().await {
        if let Ok(Ok(probe)) = result {
            measured.push(probe);
        }
    }
    // Fastest first.
    measured.sort_by(|(_, _, _, left), (_, _, _, right)| {
        right
            .as_secs_f64()
            .partial_cmp(&left.as_secs_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut sources = Vec::with_capacity(measured.len() + 1);
    let mut warming = false;
    for (url, supports_ranges, total, elapsed) in measured {
        let source = url.split('?').next().unwrap_or(&url).to_string();
        // A mirror that answers ranges but not from the middle of the archive is
        // warming this snapshot: it cannot serve chunks yet.
        let cached_ranges = if supports_ranges && total.is_some_and(|total| total > SAMPLE_SIZE * 4)
        {
            let total = total.unwrap();
            let start = total / 2;
            let end = (start + SAMPLE_SIZE - 1).min(total - 1);
            let middle_probe = async {
                let mut response = client
                    .get(&url)
                    .header(RANGE, format!("bytes={start}-{end}"))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if response.status() != StatusCode::PARTIAL_CONTENT {
                    return Err(format!("HTTP {}", response.status()));
                }
                let mut received = 0u64;
                while received < end - start + 1 {
                    let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? else {
                        break;
                    };
                    received += chunk.len() as u64;
                }
                if received == end - start + 1 {
                    Ok(())
                } else {
                    Err(format!("middle probe returned only {received} bytes"))
                }
            };
            tokio::time::timeout(Duration::from_secs(5), middle_probe)
                .await
                .is_ok_and(|result| result.is_ok())
        } else {
            false
        };
        let rate = SAMPLE_SIZE as f64 / elapsed.as_secs_f64().max(0.001);
        detail!(
            "fzv: {source} answered in {:.2}s ({:.2} MiB/s)",
            elapsed.as_secs_f64(),
            rate / 1_048_576.0
        );
        if supports_ranges && !cached_ranges {
            warming = true;
            detail!("fzv: {source} is warming this snapshot; it cannot serve chunks yet");
        }
        sources.push(Source {
            url,
            ranges: cached_ranges,
            total,
        });
    }

    if sources.is_empty() {
        eprintln!("fzv: no mirror answered; using ziglang.org");
        return Sources::new(vec![official]);
    }
    // An archive nobody has cached yet is fetched from ziglang.org, which does
    // have it, rather than from a mirror that would stream it slowly.
    if warming && env::var_os("FZV_MIRROR").is_none() {
        detail!("fzv: the mirrors are warming this archive; starting at ziglang.org");
        sources.insert(0, official.clone());
    }
    if !sources.iter().any(|source| source.url == official.url) {
        sources.push(official);
    }
    detail!(
        "fzv: {} source(s), first is {}",
        sources.len(),
        sources[0].host()
    );
    Sources::new(sources)
}

#[cfg(test)]
mod tests {
    use super::{Sources, candidate_bases, is_source_failure};
    use reqwest::StatusCode;

    #[test]
    fn classifies_failures_worth_switching_for() {
        // Reasons to fetch somewhere else.
        for status in [401, 403, 404, 410, 429, 451, 500, 502, 503, 504] {
            assert!(
                is_source_failure(StatusCode::from_u16(status).unwrap()),
                "{status} should move to the next source"
            );
        }
        // A hiccup, not a bad source.
        for status in [200, 206, 400, 416] {
            assert!(
                !is_source_failure(StatusCode::from_u16(status).unwrap()),
                "{status} should be retried on the same source"
            );
        }
    }

    #[test]
    fn reads_the_mirror_list_from_the_environment() {
        assert_eq!(
            candidate_bases(Some("https://a/zig, https://b/zig;https://c/zig"), None),
            ["https://a/zig", "https://b/zig", "https://c/zig"]
        );
        assert_eq!(
            candidate_bases(Some("https://a/zig"), Some("https://only/zig")),
            ["https://only/zig"],
            "a single mirror wins over the list"
        );
        assert_eq!(
            candidate_bases(None, Some("https://only/zig/")),
            ["https://only/zig"],
            "a trailing slash is dropped"
        );
        assert!(
            candidate_bases(None, None).len() > 8,
            "the built-in list is used by default"
        );
        assert!(
            candidate_bases(Some("  "), Some("  ")).len() > 8,
            "blank values fall back to the built-in list"
        );
    }

    #[test]
    fn walks_the_sources_in_order_and_remembers_failures() {
        let sources = Sources::new(vec![
            super::Source {
                url: "https://first.example/zig/a.zip".into(),
                ranges: true,
                total: Some(10),
            },
            super::Source {
                url: "https://second.example/zig/a.zip".into(),
                ranges: false,
                total: None,
            },
        ]);
        let hosts: Vec<String> = sources.entries().iter().map(super::Source::host).collect();
        assert_eq!(hosts, ["first.example", "second.example"]);
        assert!(sources.any_left());
        sources.mark_failed(0, "429 Too Many Requests", None);
        assert!(
            sources.any_left(),
            "the second source is still worth trying"
        );
        sources.mark_failed(1, "500", None);
        assert!(!sources.any_left());
        // A source that cannot serve chunks is not marked failed, so the
        // streaming pass can still use it.
        let only = Sources::new(vec![super::Source {
            url: "https://stream.example/zig/a.zip".into(),
            ranges: false,
            total: None,
        }]);
        assert!(only.any_left());
    }
}
