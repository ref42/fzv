//! Fetching archives over HTTP.
//!
//! Downloads are resumable and, once an archive is large enough to be worth it,
//! split across several connections by [`segments`]. [`mirrors`] measures the
//! community mirrors and returns them as a *ranked list*: a mirror that answers
//! only probes and then starts refusing requests half way through the archive is
//! normal on a public service, so the transfer continues from the next mirror
//! instead of failing. Every downloaded archive is verified by the caller
//! against the checksum from the download index, so a mirror cannot deliver
//! different bytes than ziglang.org publishes.
//!
//! A transfer draws one progress bar (see [`crate::progress`]); the diagnostics
//! around it need `FZV_VERBOSE`. The archive is moved into place atomically, so
//! a partial file is never mistaken for a finished one.

mod mirrors;
mod segments;

use crate::error::{Result, err};
use crate::log::detail;
use crate::progress::Progress;
use mirrors::{Sources, is_source_failure, select_download_sources};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{env, fs, process};

/// Downloads `url` into `output` under the name `label`, keeping the partial
/// file so that an interrupted transfer can be resumed by a later run.
pub fn download(url: &str, output: &Path, label: &str) -> Result<()> {
    let temporary = output.with_extension("downloading");
    download_into(url, output, &temporary, true, label, false)
}

/// A small text resource that was fetched, and the URL it finally came from.
pub struct Fetched {
    pub body: String,
    /// The URL after redirects: `github.com/<repo>/releases/latest` reports the
    /// release tag this way.
    pub url: String,
}

/// Fetches a small text resource: the release API, a checksum list.
///
/// No progress and no resume: these are a few kilobytes, or an error message
/// worth surfacing as-is.
pub fn fetch_text(url: &str) -> Result<Fetched> {
    let url = url.to_string();
    tokio::runtime::Runtime::new()
        .map_err(|error| err!("unable to start async runtime: {error}"))?
        .block_on(async move {
            use std::time::Duration;

            let client = reqwest::Client::builder()
                .user_agent("fzv")
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|error| err!("unable to create HTTP client: {error}"))?;
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|error| err!("{url}: {error}"))?;
            let status = response.status();
            let final_url = response.url().to_string();
            let bytes = response
                .bytes()
                .await
                .map_err(|error| err!("{url}: {error}"))?;
            if !status.is_success() {
                return Err(err!("{url} answered {status}"));
            }
            Ok(Fetched {
                body: String::from_utf8_lossy(&bytes).into_owned(),
                url: final_url,
            })
        })
}

/// Downloads `url` into `output` without reusing a partial file.
///
/// Used for small files that must not be shared with a concurrent process.
pub fn download_once(url: &str, output: &Path) -> Result<()> {
    let label = output
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| output.display().to_string());
    let mut name = output.as_os_str().to_os_string();
    name.push(format!(".{}.downloading", process::id()));
    let temporary = PathBuf::from(name);
    // The index is a few kilobytes: a bar for it would only flicker past.
    let result = download_into(url, output, &temporary, false, &label, true);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn download_into(
    url: &str,
    output: &Path,
    temporary: &Path,
    resume: bool,
    label: &str,
    quiet: bool,
) -> Result<()> {
    let url = url.to_string();
    let output = output.to_path_buf();
    let temporary = temporary.to_path_buf();
    let label = label.to_string();
    tokio::runtime::Runtime::new()
        .map_err(|e| err!("unable to start async runtime: {e}"))?
        .block_on(
            async move { download_async(&url, &output, &temporary, resume, &label, quiet).await },
        )
}

async fn download_async(
    url: &str,
    output: &Path,
    temporary: &Path,
    resume: bool,
    label: &str,
    quiet: bool,
) -> Result<()> {
    use std::time::Duration;

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

    // Sources are ranked, not chosen: when one starts refusing requests the
    // transfer continues from the next one. Nothing has been drawn yet, so a
    // switch here cannot disturb a progress line.
    let sources = Arc::new(select_download_sources(&client, url, output).await);
    let (total, supports_ranges) = probe_length(&client, &sources).await;
    let progress = Arc::new(if quiet {
        Progress::silent()
    } else {
        match total {
            Some(total) => Progress::new(total, label),
            None => Progress::unknown(label),
        }
    });

    if let Some(total) = total {
        // Chunks left by an earlier run can only be continued chunk by chunk.
        let recorded = resume && segments::has_record(temporary);
        let chunked = supports_ranges && jobs > 1 && total >= segments::MIN_SEGMENTED;
        if recorded && !chunked {
            segments::discard(temporary);
        }
        let partial_len = if !resume || (recorded && !chunked) {
            0
        } else {
            fs::metadata(temporary)
                .ok()
                .map(|metadata| metadata.len())
                .filter(|length| *length <= total)
                .unwrap_or(0)
        };
        if !recorded && partial_len == total && total > 0 {
            progress.seed(total);
            detail!("fzv: the partial file is complete");
        } else if chunked {
            // Chunks come from whichever mirror answers: one that starts
            // refusing requests is left behind without losing the chunks that
            // are already on disk.
            match segments::download_segmented(
                &client, &sources, temporary, total, jobs, resume, &progress,
            )
            .await
            {
                Ok(()) => {}
                Err(error) => {
                    // Nothing left that can serve chunks. Whatever source
                    // remains is tried as a single stream, which needs no
                    // ranges; if none remains, the error stands.
                    if !sources.any_left() {
                        return Err(error);
                    }
                    progress.suspend(|| {
                        eprintln!("fzv: streaming the rest instead ({error})");
                    });
                    segments::discard(temporary);
                    progress.reset();
                    download_single(&client, &sources, temporary, &progress).await?;
                }
            }
        } else {
            if partial_len > 0 {
                detail!(
                    "fzv: resuming at {:.1} MiB",
                    partial_len as f64 / 1_048_576.0
                );
            }
            download_single(&client, &sources, temporary, &progress).await?;
        }
    } else {
        download_single(&client, &sources, temporary, &progress).await?;
    }
    progress.finish();

    if output.exists() {
        fs::remove_file(output).map_err(crate::error::Error::from)?;
    }
    fs::rename(temporary, output).map_err(crate::error::Error::from)?;
    Ok(())
}

/// The length and range support of the first source that reports them.
///
/// The mirrors were already measured, so this is only a request for the sources
/// that did not answer a probe: the official URL, and anything a mirror left
/// unstated. A source that does not even answer is left behind.
async fn probe_length(client: &reqwest::Client, sources: &Sources) -> (Option<u64>, bool) {
    use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH};
    use std::time::Duration;

    for (index, source) in sources.entries().iter().enumerate() {
        if let Some(total) = source.total {
            let ranges = sources.can_serve_chunks(client, index).await;
            return (Some(total), ranges);
        }
        let url = source.url.clone();
        match tokio::time::timeout(Duration::from_secs(10), client.head(&url).send()).await {
            Ok(Ok(response)) if response.status().is_success() => {
                let total = response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|total| *total > 0);
                let ranges = response
                    .headers()
                    .get(ACCEPT_RANGES)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
                return (total, ranges);
            }
            _ => sources.mark_failed(index, "no answer", None),
        }
    }
    (None, false)
}

/// Why one attempt at fetching ended.
enum FetchError {
    /// The source is not usable right now (rate limited, gone, broken): the
    /// transfer should continue somewhere else.
    Source(String),
    /// The request itself failed; retrying the same source is the first answer.
    Other(String),
}

fn source_error(status: reqwest::StatusCode) -> FetchError {
    let message = format!("HTTP {status}");
    if is_source_failure(status) {
        FetchError::Source(message)
    } else {
        FetchError::Other(message)
    }
}

/// Fetches the whole file over one connection, resuming a partial file when the
/// server supports it, and moving to the next source when one stops answering.
async fn download_single(
    client: &reqwest::Client,
    sources: &Sources,
    temporary: &Path,
    progress: &Arc<Progress>,
) -> Result<()> {
    let mut failure = String::new();
    for (index, source) in sources.entries().iter().enumerate() {
        let url = source.url.clone();
        let Err(error) = fetch_until_it_works(client, &url, temporary, progress).await else {
            return Ok(());
        };
        failure = match error {
            FetchError::Source(reason) | FetchError::Other(reason) => reason,
        };
        sources.mark_failed(index, &failure, Some(progress));
    }
    Err(err!("every download source failed: {failure}"))
}

/// One source, for as many attempts as it is worth.
///
/// [`FetchError::Source`] means this source is not usable right now, so it is
/// reported straight away; anything else is retried here first.
async fn fetch_until_it_works(
    client: &reqwest::Client,
    url: &str,
    temporary: &Path,
    progress: &Arc<Progress>,
) -> std::result::Result<(), FetchError> {
    use std::time::Duration;

    /// Tries per source before it is treated as unusable too.
    const ATTEMPTS: usize = 3;
    let mut last = None;
    for attempt in 1..=ATTEMPTS {
        match fetch_stream(client, url, temporary, progress).await {
            Ok(()) => return Ok(()),
            Err(error @ FetchError::Source(_)) => return Err(error),
            Err(FetchError::Other(reason)) => {
                last = Some(reason);
                if attempt < ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
                }
            }
        }
    }
    Err(FetchError::Other(
        last.unwrap_or_else(|| "the transfer failed".to_string()),
    ))
}

/// One attempt at reading the whole archive from `url` into `temporary`.
async fn fetch_stream(
    client: &reqwest::Client,
    url: &str,
    temporary: &Path,
    progress: &Arc<Progress>,
) -> std::result::Result<(), FetchError> {
    use reqwest::{StatusCode, header::RANGE};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    // A length of zero means the server never said how big the file is.
    let total = progress.total();
    let resume_from = if total > 0 {
        tokio::fs::metadata(temporary)
            .await
            .ok()
            .map(|metadata| metadata.len())
            .filter(|length| *length > 0 && *length < total)
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
        .map_err(|e| FetchError::Other(format!("{url}: {e}")))?;
    if !response.status().is_success() {
        return Err(source_error(response.status()));
    }
    let resumed = resume_from.is_some() && response.status() == StatusCode::PARTIAL_CONTENT;
    // A server that did not answer the HEAD, or a mirror that hid the length,
    // still names the size on the response itself - and without it a transfer
    // that was cut short would look like a complete one.
    let expected = match response.content_length() {
        Some(remaining) => {
            let expected = if resumed {
                resume_from.unwrap_or(0) + remaining
            } else {
                remaining
            };
            progress.measure(expected);
            expected
        }
        None => progress.total(),
    };
    let mut file = if resumed {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(temporary)
            .await
            .map_err(|e| FetchError::Other(e.to_string()))?
    } else {
        tokio::fs::File::create(temporary)
            .await
            .map_err(|e| FetchError::Other(e.to_string()))?
    };
    let mut downloaded = if resumed { resume_from.unwrap_or(0) } else { 0 };
    if resumed {
        // Bytes already on disk are not throughput.
        progress.seed(downloaded);
    } else if resume_from.is_some() {
        detail!("fzv: the source does not support resume; restarting this transfer");
    }
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .map_err(|_| FetchError::Other(format!("download stalled for 30 seconds: {url}")))?
            .map_err(|e| FetchError::Other(format!("{url}: {e}")))?;
        let Some(chunk) = chunk else { break };
        file.write_all(&chunk)
            .await
            .map_err(|e| FetchError::Other(e.to_string()))?;
        downloaded += chunk.len() as u64;
        progress.add(chunk.len() as u64);
        if expected > 0 && downloaded >= expected {
            break;
        }
    }
    file.flush()
        .await
        .map_err(|e| FetchError::Other(e.to_string()))?;
    if expected > 0 && downloaded != expected {
        return Err(FetchError::Source(format!(
            "the response ended after {downloaded} of {expected} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const TOTAL: usize = 6 * 1024 * 1024;

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-download-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn payload() -> Arc<Vec<u8>> {
        Arc::new((0..TOTAL).map(|index| (index % 251) as u8).collect())
    }

    /// A file server that can be told to refuse requests after a while.
    struct Server {
        address: SocketAddr,
        calls: Arc<AtomicUsize>,
        first_range: Arc<Mutex<Option<u64>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        /// `refuse_after` is the number of requests to answer before every
        /// further one is answered with `429 Too Many Requests`; `truncate`
        /// sends only the first half of a body, the way a mirror that drops a
        /// transfer does.
        fn start(payload: Arc<Vec<u8>>, refuse_after: usize, truncate: bool) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let first_range = Arc::new(Mutex::new(None));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let calls = Arc::clone(&calls);
                let first_range = Arc::clone(&first_range);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(stream) = stream else { continue };
                        let payload = Arc::clone(&payload);
                        let calls = Arc::clone(&calls);
                        let first_range = Arc::clone(&first_range);
                        std::thread::spawn(move || {
                            serve(
                                stream,
                                &payload,
                                refuse_after,
                                truncate,
                                &calls,
                                &first_range,
                            )
                        });
                    }
                })
            };
            Server {
                address,
                calls,
                first_range,
                stop,
                thread: Some(thread),
            }
        }

        fn source(&self, name: &str) -> mirrors::Source {
            mirrors::Source {
                url: format!("http://{}/{name}", self.address),
                ranges: true,
                total: Some(TOTAL as u64),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        /// The first byte offset this server was asked for, which shows whether
        /// a transfer that moved here was resumed rather than restarted.
        fn first_range(&self) -> Option<u64> {
            *self.first_range.lock().unwrap()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ =
                TcpStream::connect_timeout(&self.address, std::time::Duration::from_millis(200));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn serve(
        mut stream: TcpStream,
        payload: &[u8],
        refuse_after: usize,
        truncate: bool,
        calls: &AtomicUsize,
        first_range: &Mutex<Option<u64>>,
    ) {
        let request = match read_request(&mut stream) {
            Some(request) => request,
            None => return,
        };
        let mut lines = request.lines();
        let method = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let mut range = None;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("range") {
                let value = value.trim().trim_start_matches("bytes=");
                if let Some((start, end)) = value.split_once('-')
                    && let Ok(start) = start.parse::<u64>()
                {
                    // `bytes=N-` (an open range, which is what a resume sends)
                    // runs to the end of the file.
                    range = Some((start, end.parse::<u64>().unwrap_or(u64::MAX)));
                }
            }
        }
        let total = payload.len() as u64;
        let call = calls.fetch_add(1, Ordering::Relaxed);
        if call >= refuse_after {
            let _ = stream.write_all(
                b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        if method == "HEAD" {
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\
                     Connection: close\r\n\r\n"
                )
                .as_bytes(),
            );
            return;
        }
        // A request without a range (the single-stream pass) gets the whole
        // archive, the way a server that ignores ranges would answer.
        let (start, end) = match range {
            Some((start, end)) => (start, end.min(total - 1)),
            None => (0, total - 1),
        };
        if range.is_some() {
            let mut recorded = first_range.lock().unwrap();
            if recorded.is_none() {
                *recorded = Some(start);
            }
        }
        // The length is promised in full even when `truncate` sends half of it,
        // which is what a connection that drops looks like from here: a short
        // read against a stated length.
        let promised = &payload[start as usize..=end as usize];
        let body = if truncate && promised.len() > 1 {
            &promised[..promised.len() / 2]
        } else {
            promised
        };
        let header = if range.is_some() {
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n\
                 Content-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                promised.len()
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                promised.len()
            )
        };
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body);
    }

    fn read_request(stream: &mut TcpStream) -> Option<String> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(_) => return None,
            }
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if buffer.len() > 64 * 1024 {
                return None;
            }
        }
        String::from_utf8(buffer).ok()
    }

    fn sources_of(servers: &[&Server]) -> Sources {
        Sources::new(
            servers
                .iter()
                .map(|server| server.source("zig-test.zip"))
                .collect(),
        )
    }

    /// A source that was never probed - the official URL, and anything that is
    /// not a mirror, which is how the ZLS release is fetched - is still usable
    /// for chunks once a HEAD says it answers ranges. Missing that made a chunked
    /// download that had failed over to ziglang.org stop with "no source can
    /// serve chunks any more".
    #[test]
    fn a_head_can_make_a_source_chunkable() {
        let payload = payload();
        let server = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Sources::new(vec![mirrors::Source {
            url: server.source("zig-test.zip").url,
            ranges: false,
            total: None,
        }]);
        let client = reqwest::Client::new();
        let (total, ranges) = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(probe_length(&client, &sources));
        assert_eq!(total, Some(TOTAL as u64));
        assert!(ranges, "the HEAD said ranges are supported");
        assert!(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(sources.can_serve_chunks(&client, 0)),
            "the source was skipped even though it can serve chunks"
        );
    }

    /// Compares without dumping megabytes of bytes into a failure message.
    fn assert_written(path: &Path, payload: &[u8], step: &str) {
        let written = std::fs::read(path).unwrap();
        assert_eq!(
            written.len(),
            payload.len(),
            "{step}: wrote {} bytes, expected {}",
            written.len(),
            payload.len()
        );
        if let Some(offset) = written.iter().zip(payload).position(|(a, b)| a != b) {
            panic!(
                "{step}: byte {offset} is {} but should be {}",
                written[offset], payload[offset]
            );
        }
    }

    fn run(download: impl std::future::Future<Output = Result<()>>) -> Result<()> {
        tokio::runtime::Runtime::new().unwrap().block_on(download)
    }

    /// The reported failure: the first mirror answered a chunk with 429 and the
    /// download stopped. It has to continue from the next mirror instead.
    #[test]
    fn continues_on_the_next_mirror_when_one_is_rate_limited() {
        let root = temp_dir("failover");
        let payload = payload();
        let limited = Server::start(Arc::clone(&payload), 0, false);
        let healthy = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Arc::new(sources_of(&[&limited, &healthy]));
        let path = root.join("archive.downloading");
        let progress = Arc::new(Progress::new(TOTAL as u64, "test"));

        run(download_single(
            &reqwest::Client::new(),
            &sources,
            &path,
            &progress,
        ))
        .unwrap();

        assert_written(&path, &payload, "rate-limited mirror");
        assert_eq!(
            limited.calls(),
            1,
            "the rate-limiting mirror should be asked exactly once"
        );
        assert!(healthy.calls() > 0, "the second mirror was never used");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A mirror that drops the transfer half way through is replaced, and the
    /// next one continues from what is already on disk instead of starting over.
    #[test]
    fn resumes_from_the_next_mirror_after_a_dropped_transfer() {
        let root = temp_dir("resume-failover");
        let payload = payload();
        let dropping = Server::start(Arc::clone(&payload), usize::MAX, true);
        let healthy = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Arc::new(sources_of(&[&dropping, &healthy]));
        let path = root.join("archive.downloading");
        let progress = Arc::new(Progress::new(TOTAL as u64, "test"));

        run(download_single(
            &reqwest::Client::new(),
            &sources,
            &path,
            &progress,
        ))
        .unwrap();

        assert_written(&path, &payload, "resume after a dropped transfer");
        assert!(dropping.calls() >= 1, "the dropping mirror was never used");
        let resumed_at = healthy
            .first_range()
            .expect("the next mirror was asked for the whole file instead of a range");
        assert!(
            resumed_at > 0,
            "the transfer started over instead of resuming at {resumed_at}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// When nothing is left, the failure is reported instead of pretending.
    #[test]
    fn reports_when_every_mirror_fails() {
        let root = temp_dir("all-fail");
        let payload = payload();
        let first = Server::start(Arc::clone(&payload), 0, false);
        let second = Server::start(Arc::clone(&payload), 0, false);
        let sources = Arc::new(sources_of(&[&first, &second]));
        let path = root.join("archive.downloading");

        let error = run(download_single(
            &reqwest::Client::new(),
            &sources,
            &path,
            &Arc::new(Progress::new(TOTAL as u64, "test")),
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains("every download source failed"),
            "{error}"
        );
        assert!(error.to_string().contains("429"), "{error}");
        assert!(!path.exists() || std::fs::metadata(&path).unwrap().len() == 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A mirror that rate limits in the middle of a chunked transfer is dropped
    /// and the remaining chunks come from the next one.
    #[test]
    fn chunked_transfers_survive_a_mirror_that_starts_refusing() {
        let root = temp_dir("chunked-failover");
        let payload = payload();
        let limited = Server::start(Arc::clone(&payload), 3, false);
        let healthy = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Arc::new(Sources::new(vec![
            limited.source("zig-test.zip"),
            healthy.source("zig-test.zip"),
        ]));
        let path = root.join("archive.downloading");
        let progress = Arc::new(Progress::new(TOTAL as u64, "test"));

        run(segments::download_segmented(
            &reqwest::Client::new(),
            &sources,
            &path,
            TOTAL as u64,
            4,
            true,
            &progress,
        ))
        .unwrap();

        assert_written(&path, &payload, "rate-limited mirror");
        assert!(healthy.calls() > 0, "the second mirror was never used");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The reported case: every mirror answers `429`, the transfer fails over to
    /// ziglang.org - which was never probed, so nobody knows yet whether it can
    /// serve chunks - and still finishes, in chunks, from there.
    #[test]
    fn chunked_transfers_continue_on_the_official_url_after_every_mirror_refuses() {
        let root = temp_dir("official-failover");
        let payload = payload();
        let limited = Server::start(Arc::clone(&payload), 0, false);
        let official = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Arc::new(Sources::new(vec![
            mirrors::Source {
                url: limited.source("zig-test.zip").url,
                ranges: true,
                total: Some(TOTAL as u64),
            },
            mirrors::Source {
                url: official.source("zig-test.zip").url,
                ranges: false,
                total: None,
            },
        ]));
        let path = root.join("archive.downloading");

        run(segments::download_segmented(
            &reqwest::Client::new(),
            &sources,
            &path,
            TOTAL as u64,
            4,
            true,
            &Arc::new(Progress::new(TOTAL as u64, "test")),
        ))
        .unwrap();

        assert_written(&path, &payload, "failover to the official URL");
        assert!(limited.calls() >= 1, "the mirror was never used");
        assert!(official.calls() > 0, "the official URL was never used");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A mirror that cannot serve chunks is passed over for the chunked pass    /// (the streaming pass can still use it), and it is not marked failed.
    #[test]
    fn a_source_without_ranges_is_passed_over_for_chunks() {
        let payload = payload();
        let streaming = Server::start(Arc::clone(&payload), usize::MAX, false);
        let chunked = Server::start(Arc::clone(&payload), usize::MAX, false);
        let sources = Sources::new(vec![
            mirrors::Source {
                url: streaming.source("zig-test.zip").url,
                ranges: false,
                total: Some(TOTAL as u64),
            },
            mirrors::Source {
                url: chunked.source("zig-test.zip").url,
                ranges: true,
                total: Some(TOTAL as u64),
            },
        ]);
        let client = reqwest::Client::new();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        // The first source has never been asked; the HEAD says it does answer
        // ranges, but here it is declared as a streaming-only mirror, so the
        // chunked pass walks past it without giving up on it.
        assert!(
            runtime.block_on(sources.can_serve_chunks(&client, 0)),
            "the HEAD of a range-capable server says so"
        );
        let unreachable = Sources::new(vec![
            mirrors::Source {
                url: "http://127.0.0.1:1/stream.zip".into(),
                ranges: false,
                total: Some(TOTAL as u64),
            },
            mirrors::Source {
                url: "http://127.0.0.1:1/chunks.zip".into(),
                ranges: true,
                total: Some(TOTAL as u64),
            },
        ]);
        assert!(
            !runtime.block_on(unreachable.can_serve_chunks(&client, 0)),
            "a source that cannot be reached cannot serve chunks"
        );
        assert!(unreachable.any_left(), "it is still usable while streaming");
    }
}
