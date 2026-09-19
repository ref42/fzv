//! Multi-connection downloads, handed out one chunk at a time.
//!
//! The naive way to use several connections is to give each one a fixed slice
//! of the archive, and that is precisely what makes a download 'fast at first
//! and slower and slower towards the end': the quick connections finish their
//! slice and sit idle while one laggard still has megabytes to go, so the tail
//! of the transfer runs at the speed of the slowest connection. Chunks are
//! therefore handed out from a shared counter - a connection that finishes
//! early immediately picks up the next chunk - and they are small enough that
//! the tail is a couple of seconds long at worst.
//!
//! Chunks are written into the destination file at their own offset, so there is
//! no second pass that concatenates parts (which also removes a full read and
//! write of the archive), and the chunks that are already on disk are recorded
//! in a sidecar file, so an interrupted transfer resumes instead of starting
//! over.

use super::mirrors::Sources;
use super::{FetchError, source_error};
use crate::error::{Error, Result, err};
use crate::progress::Progress;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Chunks are never smaller than this: below it the range requests themselves
/// cost more than the extra parallelism is worth.
const MIN_CHUNK: u64 = 256 * 1024;
/// Chunks are never larger than this, so that a connection picking up the last
/// one still finishes quickly.
const MAX_CHUNK: u64 = 4 * 1024 * 1024;
/// Archives smaller than this are fetched in a single request.
pub(super) const MIN_SEGMENTED: u64 = 4 * 1024 * 1024;
/// Tries per chunk before the transfer is given up.
const ATTEMPTS: usize = 3;
/// A range that delivers nothing for this long is dropped and retried.
const STALL_TIMEOUT: Duration = Duration::from_secs(20);
/// First line of the sidecar that records which chunks are on disk.
const RECORD_VERSION: &str = "fzv-chunks 1";

/// Whether a resumable record exists for this partial file.
pub(super) fn has_record(temporary: &Path) -> bool {
    sidecar(temporary).is_file()
}

/// Throws away a partial download and its record.
pub(super) fn discard(temporary: &Path) {
    let _ = std::fs::remove_file(sidecar(temporary));
    let _ = std::fs::remove_file(temporary);
}

/// The sidecar that lists the chunks already written to `temporary`.
fn sidecar(temporary: &Path) -> PathBuf {
    let mut name = temporary.as_os_str().to_os_string();
    name.push(".chunks");
    PathBuf::from(name)
}

/// Downloads `total` bytes of `url` into `temporary` over up to `jobs`
/// connections.
///
/// A record of the chunks already on disk is left behind, so the call can be
/// repeated after an interruption; the caller removes it with [`discard`] once
/// it is done with the file.
pub(super) async fn download_segmented(
    client: &reqwest::Client,
    sources: &Arc<Sources>,
    temporary: &Path,
    total: u64,
    jobs: usize,
    resume: bool,
    progress: &Arc<Progress>,
) -> Result<()> {
    let (chunk, count) = plan(total, jobs);
    let on_disk = std::fs::metadata(temporary)
        .ok()
        .map(|metadata| metadata.len());
    let recorded = resume && has_record(temporary);

    // With a record, the record says which chunks are good. Without one, only a
    // prefix of the file can be trusted, because only a single stream writes
    // from the beginning onwards.
    let state = match recorded
        .then(|| Chunks::load(temporary, total, chunk))
        .flatten()
    {
        Some(state) => state,
        None => {
            let prefix = if recorded || !resume {
                0
            } else {
                on_disk.unwrap_or(0)
            };
            Chunks::from_prefix(temporary, total, chunk, prefix)
        }
    };
    // Bytes the record does not vouch for are fetched again, not trusted.
    if on_disk.is_some_and(|length| length > state.written)
        && let Ok(file) = std::fs::OpenOptions::new().write(true).open(temporary)
    {
        let _ = file.set_len(state.written);
    }
    progress.seed(state.done_bytes());

    let file = Arc::new(
        std::fs::OpenOptions::new()
            .create(true)
            // Never truncate: the chunks already on disk are written over one
            // by one, at their own offsets.
            .truncate(false)
            .write(true)
            .open(temporary)
            .map_err(|error| err!("unable to open {}: {error}", temporary.display()))?,
    );
    let state = Arc::new(Mutex::new(state));
    let next = Arc::new(AtomicU64::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..jobs.max(1) {
        let client = client.clone();
        let sources = Arc::clone(sources);
        let file = Arc::clone(&file);
        let progress = Arc::clone(progress);
        let state = Arc::clone(&state);
        let next = Arc::clone(&next);
        tasks.spawn(async move {
            'chunks: loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    return Ok(());
                }
                if lock(&state).is_done(index) {
                    continue;
                }
                let (start, end) = chunk_range(index, chunk, total);
                // One chunk may have to come from several mirrors: a mirror that
                // starts rate limiting is left behind at once and the chunk is
                // asked for again from the next one, without losing anything
                // that is already on disk. Sources that cannot serve chunks are
                // passed over without being marked unusable - the streaming pass
                // can still use them.
                let mut candidates = sources.entries().iter().enumerate();
                let mut last = String::new();
                loop {
                    let Some((position, source)) = candidates.next() else {
                        return Err(err!("every source failed on bytes {start}-{end}: {last}"));
                    };
                    if !sources.can_serve_chunks(&client, position).await {
                        continue;
                    }
                    let url = source.url.clone();
                    let mut failure = None;
                    for attempt in 1..=ATTEMPTS {
                        match fetch_range(&client, &url, start, end, &file, &progress).await {
                            Ok(()) => {
                                failure = None;
                                break;
                            }
                            Err(FetchError::Source(reason)) => {
                                // No point in asking this one again.
                                failure = Some(reason);
                                break;
                            }
                            Err(FetchError::Other(reason)) => {
                                failure = Some(reason);
                                if attempt < ATTEMPTS {
                                    tokio::time::sleep(Duration::from_millis(250 * attempt as u64))
                                        .await;
                                }
                            }
                        }
                    }
                    let Some(reason) = failure else {
                        lock(&state).mark(index, end);
                        continue 'chunks;
                    };
                    last = reason.clone();
                    sources.mark_failed(position, &reason, Some(&progress));
                }
            }
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
    while tasks.join_next().await.is_some() {}
    if let Some(error) = failure {
        // The partial file and its record stay on disk: the next run resumes
        // from them, and the caller decides whether to start over.
        return Err(error);
    }

    let written = progress.bytes();
    if written != total {
        return Err(err!("downloaded {written} bytes, expected {total}"));
    }
    let _ = std::fs::remove_file(sidecar(temporary));
    Ok(())
}

/// The chunk size and number of chunks for a transfer: several rounds per
/// connection, so that a connection which finishes early is never idle.
fn plan(total: u64, jobs: usize) -> (u64, u64) {
    let chunk = (total / (jobs.max(1) as u64 * 8)).clamp(MIN_CHUNK, MAX_CHUNK);
    (chunk, total.div_ceil(chunk))
}

fn chunk_range(index: u64, chunk: u64, total: u64) -> (u64, u64) {
    let start = index * chunk;
    (start, (start + chunk - 1).min(total - 1))
}

/// Writes `data` at `offset` without moving a shared file cursor, so that any
/// number of connections can fill one file at the same time. On Unix this is
/// `std::os::unix::fs::FileExt::write_at`.
async fn write_at(file: &Arc<std::fs::File>, data: Vec<u8>, offset: u64) -> Result<()> {
    use std::os::windows::fs::FileExt;

    let file = Arc::clone(file);
    tokio::task::spawn_blocking(move || {
        let mut written = 0;
        while written < data.len() {
            let at = offset + written as u64;
            match file.seek_write(&data[written..], at) {
                Ok(0) => return Err(err!("unable to write the download to disk")),
                Ok(count) => written += count,
                Err(error) => return Err(Error::from(error)),
            }
        }
        Ok(())
    })
    .await
    .map_err(|error| err!("download writer stopped: {error}"))?
}

/// Fetches one byte range and appends what it delivers to `progress`.
///
/// A status that means "this source is not usable" (rate limiting, gone,
/// broken) comes back as [`FetchError::Source`] so the caller can continue at
/// the next mirror instead of retrying here.
async fn fetch_range(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    file: &Arc<std::fs::File>,
    progress: &Arc<Progress>,
) -> std::result::Result<(), FetchError> {
    use reqwest::{StatusCode, header::RANGE};

    let expected = end - start + 1;
    let mut received = 0u64;
    let result = async {
        let mut response = client
            .get(url)
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|error| FetchError::Other(format!("{error}")))?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            // A source that answers a range request with the whole file cannot
            // serve chunks: skip it at once instead of retrying it.
            if response.status().is_success() {
                return Err(FetchError::Source(format!(
                    "cannot serve byte ranges (HTTP {})",
                    response.status()
                )));
            }
            return Err(source_error(response.status()));
        }
        let mut offset = start;
        while received < expected {
            let chunk = tokio::time::timeout(STALL_TIMEOUT, response.chunk())
                .await
                .map_err(|_| {
                    FetchError::Other(format!(
                        "range {start}-{end} stalled for {} seconds",
                        STALL_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|error| FetchError::Other(format!("{error}")))?;
            let Some(chunk) = chunk else { break };
            // A response can overshoot the range it was asked for; never let it
            // spill into the next chunk.
            let take = chunk.len().min((expected - received) as usize);
            write_at(file, chunk[..take].to_vec(), offset)
                .await
                .map_err(|error| FetchError::Other(error.to_string()))?;
            offset += take as u64;
            received += take as u64;
            progress.add(take as u64);
        }
        if received != expected {
            // A short body is usually the source cutting the transfer off.
            return Err(FetchError::Source(format!(
                "range {start}-{end} returned {received} bytes, expected {expected}"
            )));
        }
        Ok(())
    }
    .await;
    if result.is_err() && received > 0 {
        // The retry must not leave this attempt's bytes in the bar.
        progress.rollback(received);
    }
    result
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Which chunks of a partial download are already on disk.
///
/// Kept in a small sidecar next to the partial file, so an interrupted transfer
/// resumes from the chunks it already has. It is a cache, not a source of
/// truth: anything that does not add up makes it start over, and the SHA-256 of
/// the finished archive is what actually decides whether the bytes are right.
struct Chunks {
    path: PathBuf,
    total: u64,
    chunk: u64,
    done: Vec<bool>,
    /// Highest byte written so far, which is also the length of the file.
    written: u64,
}

impl Chunks {
    /// Reads the record for a partial file, or `None` if there is nothing
    /// usable (wrong size, no data on disk, unparsable).
    fn load(temporary: &Path, total: u64, chunk: u64) -> Option<Chunks> {
        if chunk == 0 {
            return None;
        }
        let path = sidecar(temporary);
        let text = std::fs::read_to_string(&path).ok()?;
        let mut lines = text.lines();
        if lines.next()? != RECORD_VERSION {
            return None;
        }
        let (mut recorded_total, mut recorded_chunk, mut written) = (None, None, None);
        let mut done = Vec::new();
        for line in lines {
            let mut fields = line.split_whitespace();
            let value = fields.next()?;
            match value {
                "total" => recorded_total = Some(fields.next()?.parse::<u64>().ok()?),
                "chunk" => recorded_chunk = Some(fields.next()?.parse::<u64>().ok()?),
                "written" => written = Some(fields.next()?.parse::<u64>().ok()?),
                "done" => {
                    for index in fields {
                        done.push(index.parse::<u64>().ok()?);
                    }
                }
                _ => return None,
            }
        }
        let (recorded_total, recorded_chunk, written) =
            (recorded_total?, recorded_chunk?, written?);
        let count = total.div_ceil(chunk);
        if recorded_total != total || recorded_chunk != chunk || written > total {
            return None;
        }
        if std::fs::metadata(temporary).ok()?.len() < written {
            // The file lost the bytes the record claims are there.
            return None;
        }
        let mut done_flags = vec![false; count as usize];
        for index in done {
            let slot = done_flags.get_mut(index as usize)?;
            *slot = true;
        }
        Some(Chunks {
            path,
            total,
            chunk,
            done: done_flags,
            written,
        })
    }

    /// Starts from scratch, trusting the first `prefix` bytes. Only a prefix
    /// written by a single stream can be trusted, and only a whole number of
    /// chunks is kept: the rest is fetched again.
    fn from_prefix(temporary: &Path, total: u64, chunk: u64, prefix: u64) -> Chunks {
        let count = total.div_ceil(chunk);
        let whole = (prefix.min(total) / chunk).min(count);
        let mut done = vec![false; count as usize];
        done[..whole as usize].fill(true);
        let chunks = Chunks {
            path: sidecar(temporary),
            total,
            chunk,
            done,
            written: whole * chunk,
        };
        chunks.save();
        chunks
    }

    fn count(&self) -> u64 {
        self.done.len() as u64
    }

    fn is_done(&self, index: u64) -> bool {
        self.done.get(index as usize).copied().unwrap_or(false)
    }

    /// Bytes of the chunks that are already on disk.
    fn done_bytes(&self) -> u64 {
        (0..self.count())
            .filter(|index| self.is_done(*index))
            .map(|index| self.chunk_len(index))
            .sum()
    }

    fn chunk_len(&self, index: u64) -> u64 {
        (self.total - index * self.chunk).min(self.chunk)
    }

    /// Records a chunk as written. `end` is the last byte of the chunk, while
    /// the file length it implies is `end + 1`.
    fn mark(&mut self, index: u64, end: u64) {
        if let Some(slot) = self.done.get_mut(index as usize) {
            *slot = true;
        }
        self.written = self.written.max(end + 1);
        self.save();
    }

    /// Writing the record is best effort: it only makes the next attempt
    /// cheaper, so a failure here must not fail a transfer that is going well.
    fn save(&self) {
        let _ = std::fs::write(&self.path, self.render());
    }

    fn render(&self) -> String {
        let done: Vec<String> = self
            .done
            .iter()
            .enumerate()
            .filter(|(_, done)| **done)
            .map(|(index, _)| index.to_string())
            .collect();
        format!(
            "{RECORD_VERSION}\ntotal {}\nchunk {}\nwritten {}\ndone {}\n",
            self.total,
            self.chunk,
            self.written,
            done.join(" ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunks, Progress, Sources, download_segmented, plan, sidecar};
    use crate::error::Result;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const TOTAL: usize = 6 * 1024 * 1024;

    /// A scratch directory that removes itself.
    ///
    /// These tests write whole multi-megabyte archives, and a failing assertion
    /// leaves a directory behind if the cleanup is the last statement of the
    /// test - which is exactly when the temp directory fills up.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!(
                "fzv-download-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Scratch(root)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn payload(len: usize) -> Arc<Vec<u8>> {
        Arc::new((0..len).map(|index| (index % 251) as u8).collect())
    }

    /// A one-file HTTP server that understands range requests, for these tests
    /// only: it records every range it served in full, and can be told to drop
    /// connections after a number of successful responses.
    struct Server {
        address: SocketAddr,
        served: Arc<Mutex<Vec<(u64, u64)>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        fn start(payload: Arc<Vec<u8>>, ranges: bool, fail_after: usize) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let served: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let served = Arc::clone(&served);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut responses = 0usize;
                    for stream in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(stream) = stream else { continue };
                        let payload = Arc::clone(&payload);
                        let served = Arc::clone(&served);
                        responses += 1;
                        let fail = responses > fail_after;
                        std::thread::spawn(move || {
                            serve_one(stream, &payload, ranges, fail, &served);
                        });
                    }
                })
            };
            Server {
                address,
                served,
                stop,
                thread: Some(thread),
            }
        }

        fn url(&self, name: &str) -> String {
            format!("http://{}/{name}", self.address)
        }

        fn served(&self) -> Vec<(u64, u64)> {
            self.served.lock().unwrap().clone()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // Wake the accept loop so the thread notices.
            let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(200));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn serve_one(
        mut stream: TcpStream,
        payload: &[u8],
        ranges: bool,
        fail: bool,
        served: &Mutex<Vec<(u64, u64)>>,
    ) {
        let Some(request) = read_request(&mut stream) else {
            return;
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
                if let Some((start, end)) = value.split_once('-') {
                    range = start.parse::<u64>().ok().zip(end.parse::<u64>().ok());
                }
            }
        }

        let total = payload.len() as u64;
        if method == "HEAD" {
            let mut head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n");
            if ranges {
                head.push_str("Accept-Ranges: bytes\r\n");
            }
            let _ = stream.write_all(format!("{head}\r\n").as_bytes());
            return;
        }

        let (start, end) = match range.filter(|_| ranges) {
            Some((start, end)) => (start, end.min(total - 1)),
            None => (0, total - 1),
        };
        if fail {
            // A connection that dies mid-transfer, which is what a throttling
            // or overloaded mirror looks like from here.
            return;
        }
        let body = &payload[start as usize..=end as usize];
        let header = if range.is_some() && ranges {
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n\
                 Content-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
        };
        if stream.write_all(header.as_bytes()).is_ok() {
            served.lock().unwrap().push((start, end));
            let _ = stream.write_all(body);
        }
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

    fn run(url: &str, path: &Path, jobs: usize) -> Result<()> {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let progress = Arc::new(Progress::new(TOTAL as u64, "test"));
        let sources = Arc::new(Sources::new(vec![super::super::mirrors::Source {
            url: url.to_string(),
            ranges: true,
            total: Some(TOTAL as u64),
        }]));
        runtime.block_on(download_segmented(
            &client,
            &sources,
            path,
            TOTAL as u64,
            jobs,
            true,
            &progress,
        ))
    }

    fn bytes_of(ranges: &[(u64, u64)]) -> u64 {
        ranges.iter().map(|(start, end)| end - start + 1).sum()
    }

    /// Compares the written file with the payload without dumping megabytes of
    /// bytes into a failure message.
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

    #[test]
    fn the_archive_is_fetched_over_several_connections() {
        let payload = payload(TOTAL);
        let server = Server::start(Arc::clone(&payload), true, usize::MAX);
        let root = Scratch::new("several");
        let path = root.join("zig-test.zip.downloading");

        run(&server.url("zig-test.zip"), &path, 4).unwrap();

        assert_written(&path, &payload, "several connections");
        let served = server.served();
        assert!(
            served.len() > 1,
            "the archive was fetched in a single request: {served:?}"
        );
        assert_eq!(bytes_of(&served), TOTAL as u64, "ranges {served:?}");
        assert!(!sidecar(&path).exists(), "the record was left behind");
    }

    /// The point of the record: a transfer that died half way is continued, and
    /// the chunks already on disk are not fetched a second time.
    #[test]
    fn an_interrupted_transfer_resumes_from_its_chunks() {
        let payload = payload(TOTAL);
        let root = Scratch::new("resume");
        let path = root.join("zig-test.zip.downloading");
        let (chunk, _) = plan(TOTAL as u64, 1);

        let broken = Server::start(Arc::clone(&payload), true, 3);
        assert!(run(&broken.url("zig-test.zip"), &path, 1).is_err());
        assert!(sidecar(&path).exists(), "no record was written");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);

        let healthy = Server::start(Arc::clone(&payload), true, usize::MAX);
        run(&healthy.url("zig-test.zip"), &path, 1).unwrap();

        assert_written(&path, &payload, "resumed transfer");
        let served = healthy.served();
        assert_eq!(
            bytes_of(&served),
            TOTAL as u64 - 3 * chunk,
            "resumed over {served:?}"
        );
        assert_eq!(served.first(), Some(&(3 * chunk, 4 * chunk - 1)));
        assert!(!sidecar(&path).exists());
    }

    #[test]
    fn a_server_without_range_support_is_reported() {
        let payload = payload(TOTAL);
        let server = Server::start(Arc::clone(&payload), false, usize::MAX);
        let root = Scratch::new("single-stream");
        let path = root.join("zig-test.zip.downloading");

        // Without ranges there is nothing to segment, and the caller has to
        // fall back to one stream; here the whole request is one range anyway.
        let error = run(&server.url("zig-test.zip"), &path, 4).unwrap_err();
        assert!(
            error.message().contains("cannot serve byte ranges"),
            "unexpected error: {error}"
        );
        assert_eq!(server.served().first(), Some(&(0, TOTAL as u64 - 1)));
    }

    #[test]
    fn a_record_that_does_not_add_up_is_ignored() {
        let directory = Scratch::new("record");
        let path = directory.join("archive.downloading");
        std::fs::write(&path, vec![0u8; TOTAL]).unwrap();
        let chunk = 1024 * 1024;

        // No record yet, and a zero chunk size cannot describe anything.
        assert!(Chunks::load(&path, TOTAL as u64, chunk).is_none());
        assert!(Chunks::load(&path, TOTAL as u64, 0).is_none());

        let mut chunks = Chunks::from_prefix(&path, TOTAL as u64, chunk, 3 * chunk);
        assert_eq!(chunks.count(), 6);
        assert!(chunks.is_done(0) && chunks.is_done(2) && !chunks.is_done(3));
        assert_eq!(chunks.done_bytes(), 3 * chunk);
        // A record made for a different chunk size describes something else.
        assert!(Chunks::load(&path, TOTAL as u64, 2 * chunk).is_none());

        let loaded = Chunks::load(&path, TOTAL as u64, chunk).unwrap();
        assert_eq!(loaded.done_bytes(), 3 * chunk);
        assert_eq!(loaded.written, 3 * chunk);

        // The last chunk reaches the end of the file, and the record's high
        // water mark is a file length, not an offset.
        chunks.mark(5, TOTAL as u64 - 1);
        let loaded = Chunks::load(&path, TOTAL as u64, chunk).unwrap();
        assert_eq!(loaded.written, TOTAL as u64);
        assert_eq!(loaded.done_bytes(), 4 * chunk);
        chunks.mark(3, 4 * chunk - 1);
        chunks.mark(4, 5 * chunk - 1);
        assert_eq!(chunks.done_bytes(), TOTAL as u64);

        // A record claiming bytes that are no longer on disk cannot be trusted.
        std::fs::write(&path, vec![0u8; (chunk / 2) as usize]).unwrap();
        assert!(Chunks::load(&path, TOTAL as u64, chunk).is_none());
    }
}
