//! Fetching archives over HTTP.
//!
//! Downloads are resumable, optionally segmented across a few connections, and
//! can come from a community mirror: [`mirrors`] probes the mirrors for real
//! throughput before one is chosen. Every downloaded archive is verified by the
//! caller against the checksum from the download index, so a mirror cannot
//! deliver different bytes than ziglang.org publishes.
//!
//! Diagnostics and the progress bar go to stderr; the final archive is moved
//! into place atomically so a partial file is never mistaken for a finished one.

mod mirrors;

use crate::error::{Error, Result, err};
use mirrors::select_download_source;
use std::path::{Path, PathBuf};
use std::{env, fs, process};
/// Downloads `url` into `output`, keeping the partial file so that an
/// interrupted transfer can be resumed by a later run.
pub fn download(url: &str, output: &Path) -> Result<()> {
    let temporary = output.with_extension("downloading");
    download_into(url, output, &temporary, true)
}

/// Downloads `url` into `output` without reusing a partial file.
///
/// Used for small files that must not be shared with a concurrent process.
pub fn download_once(url: &str, output: &Path) -> Result<()> {
    let mut name = output.as_os_str().to_os_string();
    name.push(format!(".{}.downloading", process::id()));
    let temporary = PathBuf::from(name);
    let result = download_into(url, output, &temporary, false);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn download_into(url: &str, output: &Path, temporary: &Path, resume: bool) -> Result<()> {
    let url = url.to_string();
    let output = output.to_path_buf();
    let temporary = temporary.to_path_buf();
    tokio::runtime::Runtime::new()
        .map_err(|e| err!("unable to start async runtime: {e}"))?
        .block_on(async move { download_async(&url, &output, &temporary, resume).await })
}

async fn download_async(
    url: &str,
    output: &Path,
    temporary: &Path,
    resume: bool,
) -> Result<()> {
    use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;

    #[derive(Clone)]
    struct Progress {
        bytes: Arc<AtomicU64>,
        last_report_ms: Arc<AtomicU64>,
        started: Arc<Instant>,
        total: u64,
    }
    impl Progress {
        fn add(&self, count: u64) {
            let bytes = self.bytes.fetch_add(count, Ordering::Relaxed) + count;
            let elapsed_ms = self.started.elapsed().as_millis() as u64;
            let last = self.last_report_ms.load(Ordering::Relaxed);
            if elapsed_ms >= last + 250
                && self
                    .last_report_ms
                    .compare_exchange(last, elapsed_ms, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                eprint!("\r{}", self.status(bytes, elapsed_ms));
            }
        }

        fn status(&self, bytes: u64, elapsed_ms: u64) -> String {
            let fraction = (bytes as f64 / self.total.max(1) as f64).clamp(0.0, 1.0);
            let width = 24usize;
            let filled = (fraction * width as f64).round() as usize;
            let bar = format!("{}{}", "=".repeat(filled), "-".repeat(width - filled));
            let rate = bytes as f64 / (elapsed_ms.max(1) as f64 / 1000.0);
            let rate = if rate >= 1_048_576.0 {
                format!("{:.1} MiB/s", rate / 1_048_576.0)
            } else if rate >= 1024.0 {
                format!("{:.1} KiB/s", rate / 1024.0)
            } else {
                format!("{rate:.0} B/s")
            };
            format!("fzv: [{bar}] {:5.1}% ({rate})", fraction * 100.0)
        }

        fn rollback(&self, count: u64) {
            self.bytes.fetch_sub(count, Ordering::Relaxed);
        }

        fn reset(&self) {
            self.bytes.store(0, Ordering::Relaxed);
            self.last_report_ms.store(0, Ordering::Relaxed);
        }

        fn finish(&self) {
            let seconds = self.started.elapsed().as_secs_f64().max(0.001);
            let elapsed_ms = (seconds * 1000.0) as u64;
            eprintln!("\r{}", self.status(self.total, elapsed_ms));
        }
    }

    let jobs = env::var("FZV_DOWNLOAD_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 32);
    let client = reqwest::Client::builder()
        .user_agent("fzv/0.1")
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .pool_max_idle_per_host(jobs)
        .build()
        .map_err(|e| err!("unable to create HTTP client: {e}"))?;

    let (selected_url, probed_ranges, probed_total) =
        select_download_source(&client, url, output).await;
    let url = selected_url.as_str();

    let (total, head_ranges) = if let Some(total) = probed_total {
        (Some(total), false)
    } else {
        match tokio::time::timeout(Duration::from_secs(10), client.head(url).send()).await {
            Ok(Ok(response)) if response.status().is_success() => (
                response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|total| *total > 0),
                response
                    .headers()
                    .get(ACCEPT_RANGES)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.eq_ignore_ascii_case("bytes")),
            ),
            _ => (None, false),
        }
    };
    let supports_ranges = probed_ranges || head_ranges;

    if let Some(total) = total {
        let progress = Progress {
            bytes: Arc::new(AtomicU64::new(0)),
            last_report_ms: Arc::new(AtomicU64::new(0)),
            started: Arc::new(Instant::now()),
            total,
        };
        let partial_len = if resume {
            fs::metadata(temporary)
                .ok()
                .map(|metadata| metadata.len())
                .filter(|length| *length <= total)
                .unwrap_or(0)
        } else {
            0
        };
        if partial_len == total && total > 0 {
            progress.add(total);
            eprintln!("fzv: finalizing complete partial download");
        } else if partial_len > 0 {
            eprintln!(
                "fzv: resuming at {:.1} MiB",
                partial_len as f64 / 1_048_576.0
            );
            download_single(&client, url, temporary, Some(&progress)).await?;
        } else if supports_ranges && jobs > 1 && total >= 4 * 1_048_576 {
            if let Err(error) =
                download_parallel(&client, url, temporary, total, jobs, &progress).await
            {
                eprintln!("\nfzv: segmented download failed ({error}); retrying as one stream");
                progress.reset();
                download_single(&client, url, temporary, Some(&progress)).await?;
            }
        } else {
            download_single(&client, url, temporary, Some(&progress)).await?;
        }
        progress.finish();
    } else {
        download_single(&client, url, temporary, None).await?;
    }

    if output.exists() {
        fs::remove_file(output).map_err(crate::error::Error::from)?;
    }
    fs::rename(temporary, output).map_err(crate::error::Error::from)?;

    async fn download_single(
        client: &reqwest::Client,
        url: &str,
        temporary: &Path,
        progress: Option<&Progress>,
    ) -> Result<()> {
        use reqwest::{StatusCode, header::RANGE};

        let resume_from = if let Some(progress) = progress {
            tokio::fs::metadata(temporary)
                .await
                .ok()
                .map(|metadata| metadata.len())
                .filter(|length| *length > 0 && *length < progress.total)
        } else {
            None
        };
        let mut request = client.get(url);
        if let Some(offset) = resume_from {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| err!("download failed: {url}: {e}"))?;
        let resumed = resume_from.is_some() && response.status() == StatusCode::PARTIAL_CONTENT;
        response = response
            .error_for_status()
            .map_err(|e| err!("download failed: {url}: {e}"))?;
        let mut file = if resumed {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(temporary)
                .await
                .map_err(crate::error::Error::from)?
        } else {
            tokio::fs::File::create(temporary).await.map_err(crate::error::Error::from)?
        };
        let mut downloaded = if resumed { resume_from.unwrap() } else { 0 };
        if resumed {
            progress.unwrap().add(downloaded);
        } else if resume_from.is_some() {
            eprintln!("fzv: source does not support resume; restarting this transfer");
        }
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
                .await
                .map_err(|_| err!("download stalled for 30 seconds: {url}"))?
                .map_err(|e| err!("download failed: {url}: {e}"))?;
            let Some(chunk) = chunk else { break };
            file.write_all(&chunk).await.map_err(crate::error::Error::from)?;
            downloaded += chunk.len() as u64;
            if let Some(progress) = progress {
                progress.add(chunk.len() as u64);
                if downloaded >= progress.total {
                    break;
                }
            } else if downloaded % (8 * 1_048_576) < chunk.len() as u64 {
                eprint!(
                    "\rfzv: downloaded {:.1} MiB",
                    downloaded as f64 / 1_048_576.0
                );
            }
        }
        file.flush().await.map_err(crate::error::Error::from)?;
        if let Some(progress) = progress {
            if downloaded != progress.total {
                return Err(err!(
                    "download returned {downloaded} bytes, expected {}",
                    progress.total
                ));
            }
        } else {
            eprintln!();
        }
        Ok(())
    }

    async fn download_parallel(
        client: &reqwest::Client,
        url: &str,
        temporary: &Path,
        total: u64,
        jobs: usize,
        progress: &Progress,
    ) -> Result<()> {
        use reqwest::{StatusCode, header::RANGE};
        use tokio::task::JoinSet;

        let chunk_size = total.div_ceil(jobs as u64);
        let mut part_paths = Vec::with_capacity(jobs);
        let mut tasks = JoinSet::new();
        for index in 0..jobs {
            let start = index as u64 * chunk_size;
            if start >= total {
                break;
            }
            let end = (start + chunk_size - 1).min(total - 1);
            let mut part_name = temporary.as_os_str().to_os_string();
            part_name.push(format!(".part-{index}"));
            let part_path = PathBuf::from(part_name);
            part_paths.push(part_path.clone());

            let client = client.clone();
            let url = url.to_string();
            let progress = progress.clone();
            tasks.spawn(async move {
                let expected = end - start + 1;
                let mut last_error = String::new();
                for attempt in 1..=3 {
                    let mut received = 0u64;
                    let result = async {
                        let mut response = client
                            .get(&url)
                            .header(RANGE, format!("bytes={start}-{end}"))
                            .send()
                            .await
                            .map_err(|e| err!("{e}"))?;
                        if response.status() != StatusCode::PARTIAL_CONTENT {
                            return Err(err!(
                                "server returned {} for byte range {start}-{end}",
                                response.status()
                            ));
                        }
                        let mut file = tokio::fs::File::create(&part_path).await.map_err(crate::error::Error::from)?;
                        loop {
                            let chunk =
                                tokio::time::timeout(Duration::from_secs(10), response.chunk())
                                    .await
                                    .map_err(|_| err!("range stalled for 10 seconds"))?
                                    .map_err(|e| err!("{e}"))?;
                            let Some(chunk) = chunk else { break };
                            file.write_all(&chunk).await.map_err(crate::error::Error::from)?;
                            received += chunk.len() as u64;
                            progress.add(chunk.len() as u64);
                            if received >= expected {
                                break;
                            }
                        }
                        file.flush().await.map_err(crate::error::Error::from)?;
                        if received != expected {
                            return Err(err!(
                                "range {start}-{end} returned {received} bytes, expected {expected}"
                            ));
                        }
                        Ok(())
                    }
                    .await;
                    match result {
                        Ok(()) => return Ok::<_, Error>(()),
                        Err(error) => {
                            if received > 0 {
                                progress.rollback(received);
                            }
                            last_error = error.to_string();
                            if attempt < 3 {
                                tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
                            }
                        }
                    }
                }
                Err(err!(
                    "range {start}-{end} failed after 3 attempts: {last_error}"
                ))
            });
        }

        let mut failure = None;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure = Some(error);
                    tasks.abort_all();
                    break;
                }
                Err(error) => {
                    failure = Some(err!("download task failed: {error}"));
                    tasks.abort_all();
                    break;
                }
            }
        }
        if let Some(error) = failure {
            while tasks.join_next().await.is_some() {}
            for part in &part_paths {
                let _ = tokio::fs::remove_file(part).await;
            }
            return Err(error);
        }

        let mut output = tokio::fs::File::create(temporary).await.map_err(crate::error::Error::from)?;
        for part in &part_paths {
            let mut input = tokio::fs::File::open(part).await.map_err(crate::error::Error::from)?;
            tokio::io::copy(&mut input, &mut output)
                .await
                .map_err(crate::error::Error::from)?;
            tokio::fs::remove_file(part).await.map_err(crate::error::Error::from)?;
        }
        output.flush().await.map_err(crate::error::Error::from)?;
        let actual = output.metadata().await.map_err(crate::error::Error::from)?.len();
        if actual != total {
            return Err(err!(
                "assembled download is {actual} bytes, expected {total}"
            ));
        }
        Ok(())
    }

    Ok(())
}


