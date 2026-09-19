//! Updating fzv itself from its GitHub releases.
//!
//! `fzv update` asks the GitHub API which release is newest, compares its tag
//! with the version this binary was built from, downloads the archive published
//! for this platform, verifies it against the release's `SHA256SUMS`, and swaps
//! the running executable. Windows lets a running image be renamed, so the swap
//! is: move the current `fzv.exe` aside as `fzv.exe.old`, move the new one into
//! its place, and delete the leftover on the next run (this process still has it
//! mapped, so it cannot delete it here).
//!
//! When the API is unavailable (it is rate limited for anonymous requests) the
//! release *page* is followed instead: it redirects to the newest tag, and asset
//! URLs are then derived from the tag. That is why the asset names the release
//! workflow publishes are fixed by [`asset_name`].
//!
//! Every URL comes from a repository, so a fork only has to set `FZV_REPO`, and
//! the executable path is a parameter rather than `current_exe()`, which is what
//! makes the whole flow testable against a local server.

use crate::download;
use crate::error::{Result, err};
use crate::install::archive;
use crate::install::checksum;
use crate::json;
use crate::layout;
use crate::log::detail;
use std::path::{Path, PathBuf};

/// The repository fzv's releases are published from.
pub const DEFAULT_REPO: &str = "ref42/fzv";

/// Where releases and their assets live.
pub struct Releases {
    /// The API endpoint describing the newest release.
    pub api: String,
    /// The web page that redirects to it, for when the API is unavailable.
    pub web: String,
    /// Base URL of release assets: `<download>/<tag>/<name>`.
    pub download: String,
}

impl Releases {
    /// GitHub itself.
    pub fn github(repo: &str) -> Releases {
        Releases {
            api: format!("https://api.github.com/repos/{repo}/releases/latest"),
            web: format!("https://github.com/{repo}/releases/latest"),
            download: format!("https://github.com/{repo}/releases/download"),
        }
    }

    /// The repository fzv's own releases come from.
    pub fn configured() -> Releases {
        Releases::github(DEFAULT_REPO)
    }
}

/// What an update did.
#[derive(Debug)]
pub enum Outcome {
    /// The running version is already the newest release.
    Current { version: String },
    /// The executable was replaced.
    Updated {
        from: String,
        to: String,
        path: PathBuf,
    },
}

/// The newest release: its tag and the assets it offers.
struct Release {
    tag: String,
    /// Empty when the release was found through the web page rather than the
    /// API; asset URLs are then derived from the tag.
    assets: Vec<(String, String)>,
}

/// Checks for a newer release and installs it over `current_exe`.
///
/// `force` installs the newest release even when the running version is already
/// the newest one: that is how a user repairs an installation, or goes back to
/// the published build after running fzv from a working copy. The download is
/// verified the same way either way.
pub fn update(
    current_exe: &Path,
    releases: &Releases,
    current_version: &str,
    root: Option<&Path>,
    force: bool,
) -> Result<Outcome> {
    let release = latest(releases)?;
    if !force && !is_newer(&release.tag, current_version) {
        return Ok(Outcome::Current {
            version: current_version.to_string(),
        });
    }
    if force && !is_newer(&release.tag, current_version) {
        detail!(
            "fzv: {current_version} is not older than {}; installing it anyway",
            release.tag
        );
    }
    let version = release.tag.trim_start_matches('v').to_string();
    let name = asset_name(&release.tag, arch()?);
    let directory = working_directory(current_exe)?;
    let scratch = directory.join(format!(".fzv-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);

    let result = install(releases, &release, &name, &version, current_exe, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    let path = result?;

    refresh_shims(root, &path);
    Ok(Outcome::Updated {
        from: current_version.to_string(),
        to: version,
        path,
    })
}

/// Downloads the release archive, verifies it, and swaps the executable.
fn install(
    releases: &Releases,
    release: &Release,
    name: &str,
    version: &str,
    current_exe: &Path,
    scratch: &Path,
) -> Result<PathBuf> {
    if !release.assets.is_empty() && !release.assets.iter().any(|(asset, _)| asset == name) {
        return Err(err!(
            "release {} has no {name}; it offers {}",
            release.tag,
            release
                .assets
                .iter()
                .map(|(asset, _)| asset.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    std::fs::create_dir_all(scratch)
        .map_err(|error| err!("unable to create {}: {error}", scratch.display()))?;
    let archive_path = scratch.join(name);
    download::download(
        &asset_url(releases, release, name),
        &archive_path,
        &format!("fzv {version}"),
        // The release API does state an asset size, but this download is a few
        // megabytes from GitHub, which always sends a `Content-Length`.
        None,
        crate::cli::args::DEFAULT_JOBS,
    )?;
    match published_checksum(releases, release, name) {
        Some(expected) => checksum::verify_sha256(&archive_path, Some(&expected))?,
        None => detail!("fzv: the release publishes no checksum for {name}; not verifying it"),
    }

    let unpacked = scratch.join("unpacked");
    archive::extract_archive(&archive_path, &unpacked)?;
    let extracted = layout::find_named_executable(&unpacked, "fzv")?;
    let previous = swap(current_exe, &extracted)?;
    // The old image is still mapped by this process, so this usually fails; the
    // next run removes it (see `crate::cli`).
    match std::fs::remove_file(&previous) {
        Ok(()) => detail!("fzv: removed the previous binary"),
        Err(error) => detail!(
            "fzv: {} stays until this process exits ({error})",
            previous.display()
        ),
    }
    Ok(current_exe.to_path_buf())
}

/// The newest release, from the API or - when that is refused - from the release
/// page it redirects to.
fn latest(releases: &Releases) -> Result<Release> {
    let refused = match download::fetch_text(&releases.api) {
        Ok(fetched) => return parse_release(&fetched.body),
        Err(error) => error,
    };
    detail!("fzv: the release API did not answer ({refused}); asking the release page");
    // No GitHub-specific `Accept` header: the API answers JSON by default, and
    // the release *page* refuses that media type with a 406.
    let fetched = download::fetch_text(&releases.web).map_err(|web_error| {
        err!(
            "no release found ({refused}; and the release page failed as well: {web_error}); \
             is the repository public and does it have a release?"
        )
    })?;
    let tag = tag_from_release_url(&fetched.url).ok_or_else(|| {
        err!(
            "no release found ({refused}; {} names no tag); \
             is the repository public and does it have a release?",
            fetched.url
        )
    })?;
    Ok(Release {
        tag,
        assets: Vec::new(),
    })
}

/// The tag a `<repo>/releases/latest` redirect ended up at.
fn tag_from_release_url(url: &str) -> Option<String> {
    url.split("/releases/tag/")
        .nth(1)
        .map(|tag| tag.trim_end_matches('/').to_string())
        .filter(|tag| !tag.is_empty())
}

fn parse_release(body: &str) -> Result<Release> {
    let value =
        json::parse(body).map_err(|error| err!("GitHub's release answer is not JSON: {error}"))?;
    let tag = value
        .string_at("tag_name")
        .ok_or_else(|| err!("GitHub's newest release has no tag"))?
        .to_string();
    let assets = value
        .get("assets")
        .and_then(json::Value::as_array)
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| {
                    Some((
                        asset.string_at("name")?.to_string(),
                        asset.string_at("browser_download_url")?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { tag, assets })
}

/// The URL of one asset, from the release's own list when the API provided it.
fn asset_url(releases: &Releases, release: &Release, name: &str) -> String {
    release
        .assets
        .iter()
        .find(|(asset, _)| asset == name)
        .map(|(_, url)| url.clone())
        .unwrap_or_else(|| {
            format!(
                "{}/{}/{}",
                releases.download.trim_end_matches('/'),
                release.tag,
                name
            )
        })
}

/// The SHA-256 the release publishes for `name`, when it publishes any.
fn published_checksum(releases: &Releases, release: &Release, name: &str) -> Option<String> {
    let url = asset_url(releases, release, "SHA256SUMS");
    let fetched = download::fetch_text(&url).ok()?;
    parse_checksum(&fetched.body, name)
}

/// Picks `<hash>  <name>` out of a `sha256sum`-style listing.
fn parse_checksum(listing: &str, name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let file = fields.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// The archive the release workflow publishes for a platform.
fn asset_name(tag: &str, arch: &str) -> String {
    format!("fzv-{tag}-windows-{arch}.zip")
}

/// The architecture suffix used in asset names.
fn arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(err!("fzv has no release archive for {other}")),
    }
}

/// Whether `latest` is a newer version than `current`.
///
/// Tags are dotted numbers with an optional leading `v`; anything unparsable
/// counts as zero, so `v0.2.0` beats `0.1.0`.
fn is_newer(latest: &str, current: &str) -> bool {
    numbers(latest) > numbers(current)
}

fn numbers(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        })
        .collect()
}

/// The directory the running executable lives in.
fn working_directory(current_exe: &Path) -> Result<PathBuf> {
    current_exe
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| err!("unable to tell where {} lives", current_exe.display()))
}

/// Moves the running executable aside and puts `new` in its place.
///
/// Renaming the running image is allowed on Windows; overwriting it is not,
/// which is why the old file has to move out of the way first.
fn swap(current_exe: &Path, new: &Path) -> Result<PathBuf> {
    let mut name = current_exe.as_os_str().to_os_string();
    name.push(".old");
    let previous = PathBuf::from(name);
    let _ = std::fs::remove_file(&previous);
    std::fs::rename(current_exe, &previous).map_err(|error| {
        err!(
            "unable to move {} aside ({error}); is it writable?",
            current_exe.display()
        )
    })?;
    if let Err(error) = std::fs::rename(new, current_exe) {
        // Put the working binary back rather than leaving nothing behind.
        let _ = std::fs::rename(&previous, current_exe);
        return Err(err!(
            "unable to install the new fzv in {}: {error}",
            current_exe.display()
        ));
    }
    Ok(previous)
}

/// Removes the binary an update replaced, if there is one.
///
/// Called on every `fzv` run: the file could not be deleted while the process
/// that replaced it was still running.
pub fn clean_up_leftovers(current_exe: &Path) {
    let mut name = current_exe.as_os_str().to_os_string();
    name.push(".old");
    let previous = PathBuf::from(name);
    if previous.is_file() {
        match std::fs::remove_file(&previous) {
            Ok(()) => detail!("fzv: removed the replaced binary {}", previous.display()),
            Err(error) => detail!("fzv: cannot remove {} yet ({error})", previous.display()),
        }
    }
}

/// Rewrites the shims so they are the new binary too.
fn refresh_shims(root: Option<&Path>, executable: &Path) {
    if let Some(root) = root
        && let Err(error) = crate::shim::install_shims(root, true)
    {
        detail!(
            "fzv: unable to refresh the shims in {}: {error}",
            root.display()
        );
    }
    if let Some(directory) = crate::shim::install_shims_beside(executable) {
        detail!("fzv: refreshed the shims in {}", directory.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::checksum::sha256_file;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "fzv-update-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Builds the archive the release workflow publishes: a zip holding fzv.exe.
    fn release_archive(path: &Path, payload: &[u8]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zip.start_file("fzv.exe", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(payload).unwrap();
        zip.finish().unwrap().sync_all().unwrap();
    }

    /// The body the release API would answer with, pointing at `address`.
    fn api_body(tag: &str, address: SocketAddr) -> Vec<u8> {
        let name = asset_name(tag, arch().unwrap());
        format!(
            r#"{{"tag_name":"{tag}","assets":[
{{"name":"{name}","browser_download_url":"http://{address}/releases/download/{tag}/{name}"}},
{{"name":"SHA256SUMS","browser_download_url":"http://{address}/releases/download/{tag}/SHA256SUMS"}}]}}"#
        )
        .into_bytes()
    }

    /// A one-file-per-name HTTP server, for these tests only.
    struct Server {
        address: SocketAddr,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Server {
        /// The closure is given the address the server ended up with, so bodies
        /// can point back at it.
        fn start(build: impl FnOnce(SocketAddr) -> Vec<(String, Vec<u8>)>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let files = Arc::new(build(address));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(stream) = stream else { continue };
                        let files = Arc::clone(&files);
                        std::thread::spawn(move || serve(stream, &files));
                    }
                })
            };
            Server {
                address,
                stop,
                thread: Some(thread),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}/{path}", self.address)
        }

        fn releases(&self) -> Releases {
            Releases {
                api: self.url("releases/latest"),
                web: self.url("releases/latest"),
                download: format!("http://{}/releases/download", self.address),
            }
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

    /// Answers by file name: the last path segment names the body.
    fn serve(mut stream: TcpStream, files: &[(String, Vec<u8>)]) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(_) => return,
            }
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buffer).to_string();
        let wanted = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let body = files
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, body)| body.clone());
        let response = match body {
            Some(body) => {
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(&body);
                response
            }
            None => {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            }
        };
        let _ = stream.write_all(&response);
    }

    /// A release server that serves `tag`'s archive with a correct checksum.
    fn release_server(
        tag: &str,
        payload: &[u8],
        listing: impl FnOnce(&str, &str) -> String + 'static,
    ) -> (Server, String) {
        let name = asset_name(tag, arch().unwrap());
        let server = {
            let name = name.clone();
            let payload = payload.to_vec();
            Server::start(move |address| {
                // The archive is built here so its hash can go into the listing.
                let directory = temp_dir("fixture");
                let archive = directory.join(&name);
                release_archive(&archive, &payload);
                let hash = sha256_file(&archive).unwrap();
                let files = vec![
                    ("latest".to_string(), api_body(tag, address)),
                    (name.clone(), std::fs::read(&archive).unwrap()),
                    ("SHA256SUMS".to_string(), listing(&hash, &name).into_bytes()),
                ];
                let _ = std::fs::remove_dir_all(&directory);
                files
            })
        };
        (server, name)
    }

    #[test]
    fn compares_dotted_versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        // Unparsable parts count as zero rather than failing the comparison.
        assert!(is_newer("v0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("v0.2", "0.2.0"));
    }

    #[test]
    fn names_the_archive_the_workflow_publishes() {
        assert_eq!(
            asset_name("v0.2.0", "x86_64"),
            "fzv-v0.2.0-windows-x86_64.zip"
        );
        assert_eq!(
            asset_name("v0.2.0", "aarch64"),
            "fzv-v0.2.0-windows-aarch64.zip"
        );
    }

    /// Without the API, the tag comes from where the release page redirected to.
    #[test]
    fn reads_the_tag_from_the_release_page_url() {
        assert_eq!(
            tag_from_release_url("https://github.com/ref42/fzv/releases/tag/v0.2.0").as_deref(),
            Some("v0.2.0")
        );
        assert_eq!(
            tag_from_release_url("https://github.com/ref42/fzv/releases/tag/v0.2.0/").as_deref(),
            Some("v0.2.0")
        );
        assert_eq!(
            tag_from_release_url("https://github.com/ref42/fzv/releases/latest"),
            None
        );
    }

    #[test]
    fn reads_a_checksum_listing() {
        let listing = format!(
            "d2c3f1  too-short-to-be-a-hash\n\
             68659EB5F1E4EB14A1B2C3D4E5F60718293A4B5C6D7E8F9012345678901234AB  *fzv-v0.2.0-windows-x86_64.zip\n\
             {}  fzv-v0.2.0-windows-aarch64.zip\n",
            "a".repeat(64)
        );
        assert_eq!(
            parse_checksum(&listing, "fzv-v0.2.0-windows-x86_64.zip").as_deref(),
            Some("68659eb5f1e4eb14a1b2c3d4e5f60718293a4b5c6d7e8f9012345678901234ab")
        );
        assert_eq!(
            parse_checksum(&listing, "fzv-v0.2.0-windows-aarch64.zip").as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(parse_checksum(&listing, "other.zip"), None);
        // A line whose hash is not a SHA-256 does not count as verification.
        assert_eq!(parse_checksum(&listing, "too-short-to-be-a-hash"), None);
    }

    #[test]
    fn reads_the_release_the_api_describes() {
        let body = r#"{"tag_name":"v9.9.9","assets":[
            {"name":"fzv-v9.9.9-windows-x86_64.zip","browser_download_url":"https://example.invalid/a.zip"},
            {"name":"SHA256SUMS","browser_download_url":"https://example.invalid/SHA256SUMS"}]}"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.tag, "v9.9.9");
        assert_eq!(release.assets.len(), 2);
        let releases = Releases::github("ref42/fzv");
        assert_eq!(
            asset_url(&releases, &release, "fzv-v9.9.9-windows-x86_64.zip"),
            "https://example.invalid/a.zip"
        );
        assert_eq!(
            asset_url(&releases, &release, "SHA256SUMS"),
            "https://example.invalid/SHA256SUMS"
        );
        // A release found through the web page has no assets to consult, so the
        // URL is derived from the tag - which is why asset names are fixed.
        let web = Release {
            tag: "v9.9.9".to_string(),
            assets: Vec::new(),
        };
        assert_eq!(
            asset_url(&releases, &web, "SHA256SUMS"),
            "https://github.com/ref42/fzv/releases/download/v9.9.9/SHA256SUMS"
        );
        assert!(parse_release("{}").is_err(), "a release needs a tag");
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn replaces_the_executable_and_keeps_the_old_one_out_of_the_way() {
        let root = temp_dir("swap");
        let running = root.join("fzv.exe");
        let new = root.join("new-fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();

        let previous = swap(&running, &new).unwrap();
        assert_eq!(std::fs::read(&running).unwrap(), b"new");
        assert_eq!(std::fs::read(&previous).unwrap(), b"old");
        assert_eq!(
            previous.file_name().unwrap().to_string_lossy(),
            "fzv.exe.old"
        );

        clean_up_leftovers(&running);
        assert!(!previous.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The whole flow: newest release found, archive downloaded, checksum
    /// verified, executable replaced.
    #[test]
    fn updates_from_a_newer_release() {
        let root = temp_dir("update");
        let running = root.join("fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        let payload = b"the new fzv";
        let (server, _) =
            release_server("v9.9.9", payload, |hash, name| format!("{hash}  {name}\n"));

        match update(&running, &server.releases(), "0.1.0", None, false).unwrap() {
            Outcome::Updated { from, to, path } => {
                assert_eq!(from, "0.1.0");
                assert_eq!(to, "9.9.9");
                assert_eq!(path, running);
            }
            Outcome::Current { version } => panic!("expected an update, got {version}"),
        }
        assert_eq!(std::fs::read(&running).unwrap(), payload);
        assert!(
            !root.join("fzv.exe.old").exists(),
            "leftover was not removed"
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "the update left files behind"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_an_up_to_date_binary_without_downloading() {
        let root = temp_dir("current");
        let running = root.join("fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        let server = Server::start(|_| {
            vec![(
                "latest".to_string(),
                br#"{"tag_name":"v0.1.0","assets":[]}"#.to_vec(),
            )]
        });
        match update(&running, &server.releases(), "0.1.0", None, false).unwrap() {
            Outcome::Current { version } => assert_eq!(version, "0.1.0"),
            Outcome::Updated { to, .. } => panic!("nothing to update, but got {to}"),
        }
        assert_eq!(std::fs::read(&running).unwrap(), b"old");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `--force` installs the newest release even when it is the version that is
    /// already running, and verifies it the same way.
    #[test]
    fn force_reinstalls_the_running_version() {
        let root = temp_dir("force");
        let running = root.join("fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        let payload = b"the published build";
        let (server, _) =
            release_server("v0.1.0", payload, |hash, name| format!("{hash}  {name}\n"));

        // Without --force this release is not newer than the running version.
        match update(&running, &server.releases(), "0.1.0", None, false).unwrap() {
            Outcome::Current { version } => assert_eq!(version, "0.1.0"),
            Outcome::Updated { to, .. } => panic!("nothing to update, but got {to}"),
        }
        assert_eq!(std::fs::read(&running).unwrap(), b"old");

        match update(&running, &server.releases(), "0.1.0", None, true).unwrap() {
            Outcome::Updated { from, to, .. } => {
                assert_eq!(from, "0.1.0");
                assert_eq!(to, "0.1.0");
            }
            Outcome::Current { version } => panic!("--force did not install {version}"),
        }
        assert_eq!(std::fs::read(&running).unwrap(), payload);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_an_archive_whose_checksum_does_not_match() {
        let root = temp_dir("checksum");
        let running = root.join("fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        let (server, _) = release_server("v9.9.9", b"payload", |_, name| {
            format!("{}  {name}\n", "0".repeat(64))
        });

        let error = update(&running, &server.releases(), "0.1.0", None, false).unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");
        assert_eq!(
            std::fs::read(&running).unwrap(),
            b"old",
            "the binary was replaced anyway"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A release that does not carry this platform is reported instead of
    /// half-installed.
    #[test]
    fn refuses_a_release_without_this_platform() {
        let root = temp_dir("missing");
        let running = root.join("fzv.exe");
        std::fs::write(&running, b"old").unwrap();
        let release = Release {
            tag: "v9.9.9".to_string(),
            assets: vec![(
                "fzv-v9.9.9-windows-x86_64.zip".to_string(),
                "https://example.invalid/x.zip".to_string(),
            )],
        };
        let error = install(
            &Releases::github("ref42/fzv"),
            &release,
            "fzv-v9.9.9-windows-arm64.zip",
            "9.9.9",
            &running,
            &root.join("scratch"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no fzv-v9.9.9-windows-arm64.zip"),
            "{error}"
        );
        assert_eq!(std::fs::read(&running).unwrap(), b"old");
        std::fs::remove_dir_all(root).unwrap();
    }
}
