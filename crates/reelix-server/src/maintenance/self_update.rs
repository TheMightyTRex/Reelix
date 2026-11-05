//! In-app updates: how Reelix was installed, and downloading, verifying and swapping in a release.
//!
//! An install directory holds `install.json` (`{"kind": "..."}`, written by the installers and
//! release archives) next to the binary. An update downloads the release archive for this OS and
//! architecture from GitHub over HTTPS, checks it against the release's `SHA256SUMS`, unpacks it
//! into `<install dir>/.update-<version>/` and renames the new binary, `web/`, `docs/` and the
//! text files into place, keeping the replaced ones as `*.old` until the next start. Nothing else
//! in the install directory is touched (portable installs keep `data/` there).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, bail};
use reelix_api_types::system::{ReleaseAssetDto, ReleaseDto};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Marker file next to the binary; never replaced by an update.
pub const INSTALL_MARKER: &str = "install.json";
pub const BINARY: &str = if cfg!(windows) { "reelix.exe" } else { "reelix" };
pub const START_SCRIPT: &str = if cfg!(windows) {
    "start-reelix.cmd"
} else {
    "start-reelix.sh"
};
/// What an update replaces in the install directory, in this order.
pub const ITEMS: [&str; 7] = [
    BINARY,
    "web",
    "docs",
    "LICENSE.txt",
    "THIRD_PARTY_NOTICES.md",
    "README.txt",
    START_SCRIPT,
];
/// Name of the checksum asset of a release.
pub const SUMS_ASSET: &str = "SHA256SUMS";

/// Largest release archive downloaded.
pub const MAX_ARCHIVE: u64 = 300 * 1024 * 1024;
/// Largest `SHA256SUMS` read.
pub const MAX_SUMS: u64 = 64 * 1024;
/// Largest total size of the unpacked archive.
pub const MAX_EXTRACTED: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STAGING_PREFIX: &str = ".update-";

/// How Reelix was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A release zip/tarball unpacked anywhere.
    Portable,
    /// Per-user Windows installer (`%LOCALAPPDATA%\Programs\Reelix`).
    WindowsUser,
    /// All-users Windows installer, running as the `Reelix` service.
    WindowsService,
    /// `install.sh` in `/opt/reelix` with the `reelix.service` systemd unit.
    LinuxSystemd,
    Docker,
    /// Anything else, e.g. a source build.
    Source,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::WindowsUser => "windows-user",
            Self::WindowsService => "windows-service",
            Self::LinuxSystemd => "linux-systemd",
            Self::Docker => "docker",
            Self::Source => "source",
        }
    }

    fn from_marker(s: &str) -> Option<Self> {
        Some(match s {
            "portable" => Self::Portable,
            "windows-user" => Self::WindowsUser,
            "windows-service" => Self::WindowsService,
            "linux-systemd" => Self::LinuxSystemd,
            _ => return None,
        })
    }
}

/// The detected installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub kind: Kind,
    /// Directory of the binary.
    pub dir: PathBuf,
    /// Why this install can't update itself; `None` when it can.
    pub unsupported: Option<String>,
}

impl Install {
    pub fn supported(&self) -> bool {
        self.unsupported.is_none()
    }

    /// The binary an update installs (and the restart starts).
    pub fn binary(&self) -> PathBuf {
        self.dir.join(BINARY)
    }
}

#[derive(Deserialize)]
struct Marker {
    kind: String,
}

/// True inside the Docker image.
pub fn in_docker() -> bool {
    Path::new("/.dockerenv").exists() || std::env::var_os("REELIX_DOCKER").is_some()
}

/// The release archive OS name for this build.
pub(crate) fn platform() -> Option<&'static str> {
    if cfg!(windows) {
        Some("windows")
    } else if cfg!(target_os = "linux") {
        Some("linux")
    } else {
        None
    }
}

/// Works out how the binary in `dir` was installed.
pub fn detect(dir: &Path, docker: bool) -> Install {
    let install = |kind, unsupported: Option<String>| Install {
        kind,
        dir: dir.to_path_buf(),
        unsupported,
    };
    if docker {
        return install(
            Kind::Docker,
            Some(
                "Reelix runs in Docker; update by pulling the new image: docker compose pull && docker compose up -d"
                    .into(),
            ),
        );
    }
    let marker = dir.join(INSTALL_MARKER);
    let kind = match fs::read(&marker) {
        Ok(raw) => match serde_json::from_slice::<Marker>(&raw)
            .ok()
            .and_then(|m| Kind::from_marker(&m.kind))
        {
            Some(kind) => kind,
            None => {
                return install(
                    Kind::Source,
                    Some(format!("{} is not a valid install marker", marker.display())),
                );
            }
        },
        // Portable archives of 0.6.0 and older have no marker.
        Err(_)
            if (dir.join("start-reelix.cmd").is_file() || dir.join("start-reelix.sh").is_file())
                && dir.join("web").join("index.html").is_file() =>
        {
            Kind::Portable
        }
        Err(_) => {
            return install(
                Kind::Source,
                Some(
                    "this copy of Reelix wasn't installed from a release (there's no install.json next to the \
                     binary), so it can't update itself; update it the way you installed it"
                        .into(),
                ),
            );
        }
    };
    let os = platform();
    let unsupported = match (kind, os) {
        (_, None) => Some(format!(
            "there are no release builds for {}",
            std::env::consts::OS
        )),
        (Kind::WindowsUser | Kind::WindowsService, Some(os)) if os != "windows" => {
            Some(format!("install.json says {}, but this is {os}", kind.as_str()))
        }
        (Kind::LinuxSystemd, Some(os)) if os != "linux" => {
            Some(format!("install.json says {}, but this is {os}", kind.as_str()))
        }
        _ if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") => Some(format!(
            "there are no release builds for {}",
            std::env::consts::ARCH
        )),
        _ => None,
    };
    install(kind, unsupported)
}

static CURRENT: OnceLock<Option<Install>> = OnceLock::new();

/// The running binary's installation, detected once (the binary may be renamed by an update).
pub fn current() -> Option<&'static Install> {
    CURRENT
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            Some(detect(dir, in_docker()))
        })
        .as_ref()
}

/// Directory of the running binary.
pub fn install_dir() -> Option<&'static Path> {
    current().map(|i| i.dir.as_path())
}

/// Logs the install kind and removes what the last update left behind. Called early at startup.
pub async fn startup() {
    let Some(install) = current() else {
        return;
    };
    match &install.unsupported {
        None => {
            tracing::info!(kind = install.kind.as_str(), dir = %install.dir.display(), "install detected; self-update available")
        }
        Some(reason) => {
            tracing::info!(kind = install.kind.as_str(), dir = %install.dir.display(), %reason, "install detected; self-update unavailable")
        }
    }
    if matches!(install.kind, Kind::Docker | Kind::Source) {
        return;
    }
    let dir = install.dir.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || cleanup(&dir)).await {
        tracing::warn!(error = %e, "update cleanup failed");
    }
}

/// Removes `<item>.old` files and `.update-*` folders in `dir`.
pub fn cleanup(dir: &Path) {
    let mut leftovers: Vec<PathBuf> = ITEMS
        .iter()
        .map(|item| dir.join(format!("{item}.old")))
        .filter(|p| p.symlink_metadata().is_ok())
        .collect();
    if let Ok(entries) = fs::read_dir(dir) {
        leftovers.extend(
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(STAGING_PREFIX))
                .map(|e| e.path()),
        );
    }
    for path in leftovers {
        match remove_path(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "removed update leftover"),
            // e.g. the old reelix.exe while the previous process is still exiting.
            Err(e) => {
                tracing::info!(path = %path.display(), error = %e, "could not remove update leftover; retrying on the next start")
            }
        }
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    match path.symlink_metadata() {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---------- Release assets ----------

/// File name of the release archive for `os` and `arch`.
pub fn archive_name(version: &str, os: &str, arch: &str) -> String {
    let ext = if os == "windows" { "zip" } else { "tar.gz" };
    format!("reelix-{version}-{os}-{arch}.{ext}")
}

/// The assets an update of this platform needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Assets {
    pub archive: ReleaseAssetDto,
    pub sums: ReleaseAssetDto,
    /// The single top folder of the archive.
    pub top: String,
    pub format: ArchiveFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
}

/// Versions end up in file names, so only plain semver is accepted.
fn safe_version(version: &str) -> bool {
    semver::Version::parse(version).is_ok()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Picks the archive and `SHA256SUMS` of `release` for `os`/`arch`.
pub fn select_assets(release: &ReleaseDto, os: &str, arch: &str) -> Result<Assets, String> {
    if !safe_version(&release.version) {
        return Err(format!("'{}' is not a release version", release.version));
    }
    let name = archive_name(&release.version, os, arch);
    let find = |n: &str| release.assets.iter().find(|a| a.name == n).cloned();
    let archive = find(&name).ok_or_else(|| format!("release {} has no {name}", release.version))?;
    let sums = find(SUMS_ASSET).ok_or_else(|| format!("release {} has no {SUMS_ASSET}", release.version))?;
    if archive.size_bytes > MAX_ARCHIVE {
        return Err(format!("{name} is larger than {} MB", MAX_ARCHIVE >> 20));
    }
    Ok(Assets {
        archive,
        sums,
        top: format!("reelix-{}-{os}-{arch}", release.version),
        format: if os == "windows" {
            ArchiveFormat::Zip
        } else {
            ArchiveFormat::TarGz
        },
    })
}

/// [`select_assets`] for the running platform.
pub fn select_assets_here(release: &ReleaseDto) -> Result<Assets, String> {
    let os = platform().ok_or_else(|| format!("there are no release builds for {}", std::env::consts::OS))?;
    select_assets(release, os, std::env::consts::ARCH)
}

/// `SHA256SUMS` lines (`<sha256>  <file>`, `*<file>` in binary mode) as file → lowercase hash.
pub fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (hash, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*').trim();
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty())
                .then(|| (name.to_owned(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Checks `actual` (lowercase hex) against the entry for `name` in `sums`.
pub fn verify_checksum(sums: &str, name: &str, actual: &str) -> Result<(), String> {
    let expected = parse_sums(sums)
        .remove(name)
        .ok_or_else(|| format!("{SUMS_ASSET} has no entry for {name}"))?;
    if expected.eq_ignore_ascii_case(actual) {
        Ok(())
    } else {
        Err(format!(
            "the checksum of {name} doesn't match {SUMS_ASSET} (expected {expected}, got {actual}); the download was not installed"
        ))
    }
}

// ---------- Downloads ----------

/// Which download URLs are allowed.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostPolicy {
    /// Also allow plain HTTP to loopback addresses (tests only).
    pub allow_loopback_http: bool,
}

/// HTTPS to github.com or *.githubusercontent.com (where release downloads redirect to).
pub fn allowed_url(url: &reqwest::Url, policy: HostPolicy) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    match url.scheme() {
        "https" => url.port().is_none() && (host == "github.com" || host.ends_with(".githubusercontent.com")),
        "http" if policy.allow_loopback_http => {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
        _ => false,
    }
}

/// The client used for update downloads: redirects are only followed to allowed hosts.
pub fn download_client(policy: HostPolicy) -> reqwest::Result<reqwest::Client> {
    let redirects = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if allowed_url(attempt.url(), policy) {
            attempt.follow()
        } else {
            let to = attempt.url().to_string();
            attempt.error(format!("refusing to follow a redirect to {to}"))
        }
    });
    reqwest::Client::builder()
        .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .https_only(!policy.allow_loopback_http)
        // The archive must arrive byte for byte as published.
        .no_gzip()
        .redirect(redirects)
        .build()
}

async fn get(
    client: &reqwest::Client,
    url: &str,
    policy: HostPolicy,
    cap: u64,
) -> anyhow::Result<reqwest::Response> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("invalid download URL {url}"))?;
    if !allowed_url(&parsed, policy) {
        bail!("refusing to download from {url}: only HTTPS from GitHub is allowed");
    }
    let resp = client
        .get(parsed)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("downloading {url}: HTTP {}", resp.status());
    }
    if resp.content_length().is_some_and(|n| n > cap) {
        bail!("{url} is larger than {cap} bytes");
    }
    Ok(resp)
}

/// Downloads a small text file (at most `cap` bytes).
pub async fn fetch_text(
    client: &reqwest::Client,
    url: &str,
    policy: HostPolicy,
    cap: u64,
) -> anyhow::Result<String> {
    use futures::StreamExt;
    let mut stream = get(client, url, policy, cap).await?.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.with_context(|| format!("downloading {url}"))?);
        if body.len() as u64 > cap {
            bail!("{url} is larger than {cap} bytes");
        }
    }
    String::from_utf8(body).with_context(|| format!("{url} is not text"))
}

/// Streams `url` into `to` (at most `cap` bytes); returns the SHA-256 (lowercase hex).
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    policy: HostPolicy,
    cap: u64,
    to: &Path,
) -> anyhow::Result<String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut stream = get(client, url, policy, cap).await?.bytes_stream();
    let mut file = tokio::fs::File::create(to)
        .await
        .with_context(|| format!("creating {}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("downloading {url}"))?;
        written += chunk.len() as u64;
        if written > cap {
            bail!("{url} is larger than {} MB", cap >> 20);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    Ok(hex::encode(hasher.finalize()))
}

// ---------- Extraction ----------

/// The path of an archive entry inside the extraction directory, or an error for anything that
/// could land outside `top/`: absolute paths, `..`, drive letters, backslashes.
pub fn entry_path(name: &str, top: &str) -> anyhow::Result<PathBuf> {
    let unsafe_entry = || anyhow::anyhow!("the archive contains an unsafe path: {name:?}");
    if name.contains(['\\', '\0', ':']) || name.starts_with('/') {
        return Err(unsafe_entry());
    }
    let name = name.strip_prefix("./").unwrap_or(name);
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(unsafe_entry());
    }
    if parts[0] != top {
        bail!("the archive entry {name:?} is outside the expected folder {top}/");
    }
    Ok(parts.iter().collect())
}

/// Unpacked files that must be executable on Unix.
fn executable(rel: &Path, top: &str, mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & 0o111 != 0)
        || rel == Path::new(top).join("reelix")
        || rel == Path::new(top).join("start-reelix.sh")
}

struct Extractor<'a> {
    dest: &'a Path,
    top: &'a str,
    budget: u64,
    entries: usize,
}

impl Extractor<'_> {
    fn count(&mut self) -> anyhow::Result<()> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            bail!("the archive has more than {MAX_ENTRIES} entries");
        }
        Ok(())
    }

    fn dir(&mut self, name: &str) -> anyhow::Result<()> {
        self.count()?;
        let rel = entry_path(name, self.top)?;
        fs::create_dir_all(self.dest.join(rel))?;
        Ok(())
    }

    fn file(&mut self, name: &str, mode: Option<u32>, reader: &mut dyn Read) -> anyhow::Result<()> {
        self.count()?;
        let rel = entry_path(name, self.top)?;
        if rel.parent().is_none_or(|p| p.as_os_str().is_empty()) {
            bail!("the archive entry {name:?} is not inside {}/", self.top);
        }
        let path = self.dest.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // The destination is a fresh folder, so an existing file means a duplicate entry.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("unpacking {name}"))?;
        let copied = io::copy(&mut reader.take(self.budget + 1), &mut file)?;
        if copied > self.budget {
            bail!("the archive unpacks to more than {} MB", MAX_EXTRACTED >> 20);
        }
        self.budget -= copied;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable(&rel, self.top, mode) {
                0o755
            } else {
                0o644
            };
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = (mode, executable);
        Ok(())
    }
}

/// Unpacks `archive` into `dest` (a fresh directory) and returns `dest/top`. Only regular files
/// and directories under `top/` are accepted; links and special files fail the whole update.
pub fn extract(archive: &Path, format: ArchiveFormat, dest: &Path, top: &str) -> anyhow::Result<PathBuf> {
    let mut x = Extractor {
        dest,
        top,
        budget: MAX_EXTRACTED,
        entries: 0,
    };
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(file).context("the download is not a zip file")?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                let name = entry.name()?.into_owned();
                if entry.is_symlink() {
                    bail!("the archive contains a link: {name:?}");
                } else if entry.is_dir() {
                    x.dir(&name)?;
                } else if entry.is_file() {
                    let mode = entry.unix_mode();
                    x.file(&name, mode, &mut entry)?;
                } else {
                    bail!("the archive contains an unsupported entry: {name:?}");
                }
            }
        }
        ArchiveFormat::TarGz => {
            use tar::EntryType;
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
            for entry in tar.entries().context("the download is not a .tar.gz file")? {
                let mut entry = entry.context("reading the archive")?;
                let name = String::from_utf8(entry.path_bytes().into_owned())
                    .map_err(|_| anyhow::anyhow!("the archive contains a non-UTF-8 path"))?;
                match entry.header().entry_type() {
                    EntryType::Directory => x.dir(&name)?,
                    EntryType::Regular | EntryType::Continuous => {
                        let mode = entry.header().mode().ok();
                        x.file(&name, mode, &mut entry)?;
                    }
                    // PAX metadata for the following entry.
                    EntryType::XHeader | EntryType::XGlobalHeader => {}
                    EntryType::Symlink | EntryType::Link => bail!("the archive contains a link: {name:?}"),
                    other => bail!("the archive contains an unsupported entry ({other:?}): {name:?}"),
                }
            }
        }
    }
    let root = dest.join(top);
    if !root.join(BINARY).is_file() || !root.join("web").join("index.html").is_file() {
        bail!("the archive doesn't contain {top}/{BINARY} and {top}/web/");
    }
    Ok(root)
}

/// The staging folder for `version` inside the install directory.
pub fn staging_dir(install_dir: &Path, version: &str) -> PathBuf {
    install_dir.join(format!("{STAGING_PREFIX}{version}"))
}

// ---------- Swap ----------

/// Renames, retrying briefly on Windows where virus scanners and the web server may still hold a
/// handle inside `web/`.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Err(e) if cfg!(windows) && attempt < 10 && e.kind() == io::ErrorKind::PermissionDenied => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            other => return other,
        }
    }
}

/// Moves the staged [`ITEMS`] from `staged` into `install_dir`, keeping the replaced ones as
/// `<item>.old`. On failure everything already moved is put back.
pub fn swap(install_dir: &Path, staged: &Path) -> anyhow::Result<()> {
    swap_with(install_dir, staged, &ITEMS, &mut rename)
}

struct Step {
    item: &'static str,
    /// The current item was renamed to `.old`.
    kept_old: bool,
    /// The staged item was renamed into place.
    placed: bool,
}

fn swap_with(
    dir: &Path,
    staged: &Path,
    items: &[&'static str],
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
) -> anyhow::Result<()> {
    let mut done = Vec::new();
    let Err(e) = swap_steps(dir, staged, items, rename, &mut done) else {
        return Ok(());
    };
    let mut failed = Vec::new();
    for step in done.iter().rev() {
        let current = dir.join(step.item);
        if step.placed
            && let Err(e) = rename(&current, &staged.join(step.item))
        {
            failed.push(format!("{}: {e}", step.item));
            continue;
        }
        if step.kept_old
            && let Err(e) = rename(&dir.join(format!("{}.old", step.item)), &current)
        {
            failed.push(format!("{}.old: {e}", step.item));
        }
    }
    if failed.is_empty() {
        Err(e.context("installing the update failed; the previous files were put back"))
    } else {
        Err(e.context(format!(
            "installing the update failed and putting back the previous files failed too ({}); rename the .old files in {} back by hand",
            failed.join(", "),
            dir.display()
        )))
    }
}

fn swap_steps(
    dir: &Path,
    staged: &Path,
    items: &[&'static str],
    rename: &mut dyn FnMut(&Path, &Path) -> io::Result<()>,
    done: &mut Vec<Step>,
) -> anyhow::Result<()> {
    for &item in items {
        let new = staged.join(item);
        if new.symlink_metadata().is_err() {
            continue;
        }
        let current = dir.join(item);
        let old = dir.join(format!("{item}.old"));
        remove_path(&old).with_context(|| format!("removing {}", old.display()))?;
        let exists = current.symlink_metadata().is_ok();
        if exists {
            rename(&current, &old).with_context(|| format!("moving {} aside", current.display()))?;
        }
        done.push(Step {
            item,
            kept_old: exists,
            placed: false,
        });
        rename(&new, &current).with_context(|| format!("moving the new {item} into place"))?;
        if let Some(step) = done.last_mut() {
            step.placed = true;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) const TOP: &str = "reelix-9.9.9-linux-x86_64";

    #[test]
    fn detects_install_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let source = detect(dir, false);
        assert_eq!(source.kind, Kind::Source);
        assert!(source.unsupported.as_deref().unwrap().contains("install.json"));
        assert_eq!(detect(dir, true).kind, Kind::Docker);
        assert!(!detect(dir, true).supported());

        // 0.6.0 portable archives: a start script and the web UI, no marker.
        fs::write(dir.join("start-reelix.sh"), "").unwrap();
        assert_eq!(
            detect(dir, false).kind,
            Kind::Source,
            "the web UI is required too"
        );
        fs::create_dir(dir.join("web")).unwrap();
        fs::write(dir.join("web/index.html"), "").unwrap();
        let portable = detect(dir, false);
        assert_eq!(portable.kind, Kind::Portable);
        // Only OSes with release builds (Windows, Linux) can update themselves.
        assert_eq!(portable.supported(), platform().is_some(), "{portable:?}");
        assert_eq!(portable.binary(), dir.join(BINARY));

        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "linux-systemd"}"#).unwrap();
        let systemd = detect(dir, false);
        assert_eq!(systemd.kind, Kind::LinuxSystemd);
        assert_eq!(systemd.supported(), cfg!(target_os = "linux"));
        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "windows-service"}"#).unwrap();
        let service = detect(dir, false);
        assert_eq!(service.kind, Kind::WindowsService);
        assert_eq!(service.supported(), cfg!(windows));
        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "windows-user"}"#).unwrap();
        assert_eq!(detect(dir, false).kind, Kind::WindowsUser);
        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "portable"}"#).unwrap();
        assert_eq!(detect(dir, false).kind, Kind::Portable);
        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "flatpak"}"#).unwrap();
        let bad = detect(dir, false);
        assert_eq!(bad.kind, Kind::Source);
        assert!(!bad.supported());
    }

    fn release(version: &str, assets: &[&str]) -> ReleaseDto {
        ReleaseDto {
            version: version.into(),
            name: version.into(),
            notes: String::new(),
            url: String::new(),
            published_at: None,
            prerelease: false,
            newer: true,
            assets: assets
                .iter()
                .map(|n| ReleaseAssetDto {
                    name: (*n).into(),
                    url: format!("https://github.com/x/{n}"),
                    size_bytes: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn selects_assets_for_the_platform() {
        assert_eq!(
            archive_name("1.2.3", "windows", "x86_64"),
            "reelix-1.2.3-windows-x86_64.zip"
        );
        assert_eq!(
            archive_name("1.2.3", "linux", "aarch64"),
            "reelix-1.2.3-linux-aarch64.tar.gz"
        );
        let r = release(
            "1.2.3",
            &[
                "reelix-1.2.3-windows-x86_64.zip",
                "reelix-1.2.3-linux-x86_64.tar.gz",
                "reelix-1.2.3-linux-aarch64.tar.gz",
                SUMS_ASSET,
            ],
        );
        let a = select_assets(&r, "linux", "aarch64").unwrap();
        assert_eq!(a.archive.name, "reelix-1.2.3-linux-aarch64.tar.gz");
        assert_eq!(a.sums.name, SUMS_ASSET);
        assert_eq!(a.top, "reelix-1.2.3-linux-aarch64");
        assert_eq!(a.format, ArchiveFormat::TarGz);
        assert_eq!(
            select_assets(&r, "windows", "x86_64").unwrap().format,
            ArchiveFormat::Zip
        );
        assert!(
            select_assets(&r, "windows", "aarch64")
                .unwrap_err()
                .contains("has no")
        );
        let no_sums = release("1.2.3", &["reelix-1.2.3-linux-x86_64.tar.gz"]);
        assert!(
            select_assets(&no_sums, "linux", "x86_64")
                .unwrap_err()
                .contains(SUMS_ASSET)
        );
        let odd = release("../1.2.3", &[SUMS_ASSET]);
        assert!(select_assets(&odd, "linux", "x86_64").is_err());
    }

    #[test]
    fn parses_and_checks_sha256sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let text = format!(
            "{a}  reelix-1.0.0-linux-x86_64.tar.gz\n{b} *reelix-1.0.0-windows-x86_64.zip\n\ngarbage line\n"
        );
        let sums = parse_sums(&text);
        assert_eq!(sums.len(), 2);
        assert_eq!(sums["reelix-1.0.0-windows-x86_64.zip"], "b".repeat(64));
        assert!(verify_checksum(&text, "reelix-1.0.0-linux-x86_64.tar.gz", &a).is_ok());
        assert!(verify_checksum(&text, "reelix-1.0.0-windows-x86_64.zip", &"b".repeat(64)).is_ok());
        let err = verify_checksum(&text, "reelix-1.0.0-linux-x86_64.tar.gz", &"c".repeat(64)).unwrap_err();
        assert!(err.contains("doesn't match"), "{err}");
        assert!(
            verify_checksum(&text, "other.zip", &a)
                .unwrap_err()
                .contains("no entry")
        );
    }

    #[test]
    fn only_github_over_https_is_allowed() {
        let ok = |u: &str, p| allowed_url(&reqwest::Url::parse(u).unwrap(), p);
        let strict = HostPolicy::default();
        let tests = HostPolicy {
            allow_loopback_http: true,
        };
        assert!(ok(
            "https://github.com/demonzoned/Reelix/releases/download/v1/x.zip",
            strict
        ));
        assert!(ok("https://objects.githubusercontent.com/x", strict));
        assert!(ok("https://release-assets.githubusercontent.com/x", strict));
        assert!(!ok("http://github.com/x", strict));
        assert!(!ok("https://github.com.evil.example/x", strict));
        assert!(!ok("https://evilgithubusercontent.com/x", strict));
        assert!(!ok("https://github.com:8443/x", strict));
        assert!(!ok("http://127.0.0.1:9/x", strict));
        assert!(ok("http://127.0.0.1:9/x", tests));
        assert!(ok("http://[::1]:9/x", tests));
        assert!(!ok("http://10.0.0.1/x", tests));
        assert!(!ok("file:///etc/passwd", tests));
    }

    /// A release zip with `entries` (name, contents); names ending in `/` are directories.
    pub(crate) fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts.unix_permissions(0o644)).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    /// A release .tar.gz; `mode` per file, names ending in `/` are directories.
    pub(crate) fn tar_with(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let gz = flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (name, data, mode) in entries {
            let mut h = tar::Header::new_gnu();
            if name.ends_with('/') {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_size(0);
            } else {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_size(data.len() as u64);
            }
            h.set_mode(*mode);
            // `append_data` would reject `..`; write the raw name like a hostile archive would.
            let raw = &mut h.as_old_mut().name;
            raw.fill(0);
            raw[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            tar.append(&h, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    fn release_files(top: &str) -> Vec<(String, &'static [u8])> {
        vec![
            (format!("{top}/"), b"".as_slice()),
            (format!("{top}/reelix"), b"new binary".as_slice()),
            (format!("{top}/reelix.exe"), b"new binary".as_slice()),
            (format!("{top}/web/index.html"), b"<html>new</html>".as_slice()),
            (format!("{top}/start-reelix.sh"), b"#!/bin/sh".as_slice()),
            (
                format!("{top}/install.json"),
                br#"{"kind": "portable"}"#.as_slice(),
            ),
        ]
    }

    #[test]
    fn extracts_zip_and_rejects_unsafe_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let top = "reelix-9.9.9-windows-x86_64";
        let good = tmp.path().join("good.zip");
        let files = release_files(top);
        let entries: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        zip_with(&good, &entries);
        let dest = tmp.path().join("out");
        let root = extract(&good, ArchiveFormat::Zip, &dest, top).unwrap();
        assert_eq!(
            fs::read(root.join("web/index.html")).unwrap(),
            b"<html>new</html>"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("reelix"), 0o755, "the binary is executable");
            assert_eq!(mode("start-reelix.sh"), 0o755);
            assert_eq!(mode("web/index.html"), 0o644);
        }

        let bad_zip = |name: &str, entries: &[(&str, &[u8])]| {
            let path = tmp.path().join(name);
            zip_with(&path, entries);
            let dest = tmp.path().join(format!("{name}.out"));
            extract(&path, ArchiveFormat::Zip, &dest, top)
                .unwrap_err()
                .to_string()
        };
        let dotdot = format!("{top}/../evil.txt");
        assert!(bad_zip("dotdot.zip", &[(&dotdot, b"x")]).contains("unsafe"));
        assert!(bad_zip("abs.zip", &[("/etc/evil", b"x")]).contains("unsafe"));
        assert!(bad_zip("other.zip", &[("elsewhere/reelix", b"x")]).contains("outside"));
        assert!(bad_zip("empty.zip", &[]).contains("doesn't contain"));
        assert!(!tmp.path().join("evil.txt").exists());

        let link = tmp.path().join("link.zip");
        let mut zip = zip::ZipWriter::new(File::create(&link).unwrap());
        zip.add_symlink(
            format!("{top}/web"),
            "/etc",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.finish().unwrap();
        let err = extract(&link, ArchiveFormat::Zip, &tmp.path().join("l.out"), top).unwrap_err();
        assert!(err.to_string().contains("link"), "{err}");
    }

    #[test]
    fn extracts_tar_gz_and_rejects_unsafe_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.tar.gz");
        let files = release_files(TOP);
        let entries: Vec<(&str, &[u8], u32)> = files
            .iter()
            .map(|(n, d)| (n.as_str(), *d, if n.ends_with("/reelix") { 0o755 } else { 0o644 }))
            .collect();
        tar_with(&good, &entries);
        let root = extract(&good, ArchiveFormat::TarGz, &tmp.path().join("out"), TOP).unwrap();
        assert_eq!(fs::read(root.join("reelix")).unwrap(), b"new binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("reelix"), 0o755);
            assert_eq!(mode("start-reelix.sh"), 0o755);
            assert_eq!(mode("install.json"), 0o644);
        }

        let bad_tar = |name: &str, entries: &[(&str, &[u8], u32)]| {
            let path = tmp.path().join(name);
            tar_with(&path, entries);
            extract(
                &path,
                ArchiveFormat::TarGz,
                &tmp.path().join(format!("{name}.out")),
                TOP,
            )
            .unwrap_err()
            .to_string()
        };
        let dotdot = format!("{TOP}/../../evil.txt");
        assert!(bad_tar("dotdot.tar.gz", &[(&dotdot, b"x", 0o644)]).contains("unsafe"));
        assert!(bad_tar("abs.tar.gz", &[("/tmp/evil", b"x", 0o644)]).contains("unsafe"));

        for (kind, file) in [(tar::EntryType::Symlink, "sym"), (tar::EntryType::Link, "hard")] {
            let path = tmp.path().join(format!("{file}.tar.gz"));
            let gz = flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(kind);
            h.set_size(0);
            tar.append_link(&mut h, format!("{TOP}/web"), "/etc").unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            let err = extract(
                &path,
                ArchiveFormat::TarGz,
                &tmp.path().join(format!("{file}.out")),
                TOP,
            )
            .unwrap_err();
            assert!(err.to_string().contains("link"), "{file}: {err}");
        }
    }

    fn install_dir_with(dir: &Path, version: &str) {
        fs::write(dir.join(BINARY), format!("binary {version}")).unwrap();
        fs::create_dir_all(dir.join("web")).unwrap();
        fs::write(dir.join("web/index.html"), version).unwrap();
        fs::write(dir.join(INSTALL_MARKER), r#"{"kind": "linux-systemd"}"#).unwrap();
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join("data/reelix.db"), "db").unwrap();
    }

    #[test]
    fn swaps_files_and_rolls_back_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("install");
        let staged = tmp.path().join("staged");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&staged).unwrap();
        install_dir_with(&dir, "old");
        install_dir_with(&staged, "new");
        fs::write(staged.join(INSTALL_MARKER), r#"{"kind": "portable"}"#).unwrap();
        fs::write(staged.join("README.txt"), "new readme").unwrap();
        fs::write(dir.join("web.old"), "stale leftover").unwrap();

        // The fourth rename (moving the new web/ into place) fails: everything is put back.
        let mut calls = 0;
        let mut failing = |a: &Path, b: &Path| {
            calls += 1;
            if calls == 4 {
                Err(io::Error::other("disk on fire"))
            } else {
                fs::rename(a, b)
            }
        };
        let err = swap_with(&dir, &staged, &ITEMS, &mut failing).unwrap_err();
        assert!(format!("{err:#}").contains("put back"), "{err:#}");
        assert_eq!(fs::read_to_string(dir.join(BINARY)).unwrap(), "binary old");
        assert_eq!(fs::read_to_string(dir.join("web/index.html")).unwrap(), "old");
        assert_eq!(fs::read_to_string(staged.join(BINARY)).unwrap(), "binary new");
        assert!(!dir.join(format!("{BINARY}.old")).exists());

        swap(&dir, &staged).unwrap();
        assert_eq!(fs::read_to_string(dir.join(BINARY)).unwrap(), "binary new");
        assert_eq!(fs::read_to_string(dir.join("web/index.html")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dir.join("README.txt")).unwrap(), "new readme");
        assert_eq!(
            fs::read_to_string(dir.join(INSTALL_MARKER)).unwrap(),
            r#"{"kind": "linux-systemd"}"#,
            "the install marker is kept"
        );
        assert_eq!(fs::read_to_string(dir.join("data/reelix.db")).unwrap(), "db");
        assert_eq!(
            fs::read_to_string(dir.join(format!("{BINARY}.old"))).unwrap(),
            "binary old"
        );
        assert!(dir.join("web.old/index.html").is_file());

        fs::create_dir_all(staging_dir(&dir, "9.9.9")).unwrap();
        cleanup(&dir);
        assert!(!dir.join(format!("{BINARY}.old")).exists());
        assert!(!dir.join("web.old").exists());
        assert!(!staging_dir(&dir, "9.9.9").exists());
        assert!(dir.join("data/reelix.db").is_file());
    }
}
