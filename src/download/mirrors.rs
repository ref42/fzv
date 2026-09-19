//! Choosing a Zig mirror.
//!
//! Latency alone is a poor selector: a mirror can answer a tiny request quickly
//! and then deliver the archive at dial-up speed. Each candidate is therefore
//! measured for throughput on a 256 KiB sample, and a mirror that looks like it
//! is still warming the snapshot falls back to ziglang.org.

use std::env;
use std::path::Path;
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

pub(super) async fn select_download_source(
    client: &reqwest::Client,
    official_url: &str,
    output: &Path,
) -> (String, bool, Option<u64>) {
    use reqwest::{
        StatusCode,
        header::{CONTENT_RANGE, RANGE},
    };
    use std::time::{Duration, Instant};
    use tokio::task::JoinSet;

    let Some(filename) = output.file_name().and_then(|name| name.to_str()) else {
        return (official_url.to_string(), false, None);
    };
    // Only Zig archives are mirrored; the index and the ZLS release are not.
    if !filename.starts_with("zig-")
        || !filename.ends_with(".zip")
        || env::var_os("FZV_NO_MIRRORS").is_some()
    {
        return (official_url.to_string(), false, None);
    }

    let mut candidates = Vec::with_capacity(ZIG_MIRRORS.len() + 1);
    if let Ok(base) = env::var("FZV_MIRROR") {
        candidates.push(format!(
            "{}/{}?source=fzv",
            base.trim_end_matches('/'),
            filename
        ));
    } else {
        for base in ZIG_MIRRORS {
            candidates.push(format!(
                "{}/{}?source=fzv",
                base.trim_end_matches('/'),
                filename
            ));
        }
    }
    candidates.push(official_url.to_string());

    // Latency alone is a poor mirror selector: a mirror can answer a tiny
    // request quickly and then deliver the archive at dial-up speed. Measure
    // enough data to compare throughput instead.
    const SAMPLE_SIZE: u64 = 256 * 1024;
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
                    return Err(format!("probe returned HTTP {}", response.status()));
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

    let mut successful = Vec::new();
    while let Some(result) = probes.join_next().await {
        if let Ok(Ok((url, supports_ranges, total, elapsed))) = result {
            successful.push((url, supports_ranges, total, elapsed));
        }
    }
    if let Some((url, supports_ranges, total, elapsed)) =
        successful
            .into_iter()
            .max_by(|(_, _, _, left_elapsed), (_, _, _, right_elapsed)| {
                let left_rate = SAMPLE_SIZE as f64 / left_elapsed.as_secs_f64().max(0.001);
                let right_rate = SAMPLE_SIZE as f64 / right_elapsed.as_secs_f64().max(0.001);
                left_rate
                    .partial_cmp(&right_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    {
        let source = url.split('?').next().unwrap_or(&url);
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
        let rate = if rate >= 1_048_576.0 {
            format!("{:.1} MiB/s", rate / 1_048_576.0)
        } else {
            format!("{:.0} KiB/s", rate / 1024.0)
        };
        if supports_ranges
            && !cached_ranges
            && url != official_url
            && env::var_os("FZV_MIRROR").is_none()
        {
            eprintln!("fzv: {source} is warming the archive ({rate}); falling back to ziglang.org");
            return (official_url.to_string(), false, None);
        }
        eprintln!("fzv: selected {source} ({rate} probe throughput)");
        if supports_ranges && !cached_ranges {
            eprintln!("fzv: mirror is warming this snapshot; using one streaming request");
        }
        return (url, cached_ranges, total);
    }

    eprintln!("fzv: all mirror probes failed; using ziglang.org");
    (official_url.to_string(), false, None)
}
