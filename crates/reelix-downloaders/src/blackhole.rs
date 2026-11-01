//! Torrent and Usenet blackhole: drop `.torrent`/`.nzb` files into a folder watched by
//! another program, then pick finished downloads up from a completed ("watch") folder.
//!
//! Download ids are the file/folder names (release title, sanitised). An entry in the watch
//! folder counts as completed once nothing in it changed for `settle_seconds`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::Value;

use crate::common::{self, CommonSettings, de_num};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::{BuildContext, ClientError, DownloadClient};

pub const TORRENT_IMPLEMENTATION: &str = "torrent_blackhole";
pub const USENET_IMPLEMENTATION: &str = "usenet_blackhole";
/// Category reported for items found in `book_folder`.
pub const BOOK_CATEGORY: &str = "books";
/// Category reported for items found in `music_folder`.
pub const MUSIC_CATEGORY: &str = "music";

#[derive(Debug, Clone, Deserialize)]
pub struct BlackholeSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    /// Where `.torrent`/`.nzb` files are written.
    pub drop_folder: PathBuf,
    /// Where the external program puts finished downloads.
    pub watch_folder: PathBuf,
    /// Optional watch folder for finished ebook/audiobook downloads.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub book_folder: Option<PathBuf>,
    /// Optional watch folder for finished music downloads.
    #[serde(default, deserialize_with = "de_opt_path")]
    pub music_folder: Option<PathBuf>,
    /// Torrent only: write magnets as `<title>.magnet` files.
    #[serde(default)]
    pub save_magnet_files: bool,
    #[serde(default = "magnet_ext")]
    pub magnet_extension: String,
    /// Files must be copied (the external client keeps seeding them).
    #[serde(default)]
    pub read_only: bool,
    #[serde(deserialize_with = "de_num")]
    pub settle_seconds: u64,
}

fn magnet_ext() -> String {
    ".magnet".into()
}

/// Empty string / null → `None`.
fn de_opt_path<'de, D>(d: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from))
}

fn base_fields(default_name: &str, ext: &str) -> Vec<FieldSchema> {
    vec![
        common::f_name(default_name),
        FieldSchema::new("drop_folder", &format!("{ext} folder"), FieldKind::Path)
            .required()
            .help(&format!("Reelix writes {ext} files here")),
        FieldSchema::new("watch_folder", "Watch folder", FieldKind::Path)
            .required()
            .help("Finished downloads appear here and are imported from it"),
        FieldSchema::new("book_folder", "Book watch folder", FieldKind::Path)
            .help("Finished ebook/audiobook downloads appear here. Empty = use the watch folder")
            .advanced(),
        FieldSchema::new("music_folder", "Music watch folder", FieldKind::Path)
            .help("Finished music downloads appear here. Empty = use the watch folder")
            .advanced(),
    ]
}

fn tail(mut fields: Vec<FieldSchema>) -> Vec<FieldSchema> {
    fields.push(
        FieldSchema::new("settle_seconds", "Settle time (seconds)", FieldKind::Number)
            .default_value(60)
            .help("An item is complete once nothing in it changed for this long")
            .advanced(),
    );
    fields.extend(common::f_common_tail());
    fields
}

pub fn torrent_schema() -> ProviderSchema {
    let mut f = base_fields("Torrent Blackhole", ".torrent");
    f.push(
        FieldSchema::new("save_magnet_files", "Save magnet files", FieldKind::Bool)
            .default_value(false)
            .help("Write magnet links to files (otherwise magnet-only releases are rejected)"),
    );
    f.push(
        FieldSchema::new("magnet_extension", "Magnet file extension", FieldKind::Text)
            .default_value(".magnet")
            .advanced(),
    );
    f.push(
        FieldSchema::new("read_only", "Read only", FieldKind::Bool)
            .default_value(true)
            .help("Copy instead of moving files, so the torrent client can keep seeding"),
    );
    ProviderSchema {
        implementation: TORRENT_IMPLEMENTATION.into(),
        name: "Torrent Blackhole".into(),
        description: Some("Writes .torrent files to a folder watched by another client".into()),
        info_url: None,
        fields: tail(f),
    }
}

pub fn usenet_schema() -> ProviderSchema {
    let f = base_fields("Usenet Blackhole", ".nzb");
    ProviderSchema {
        implementation: USENET_IMPLEMENTATION.into(),
        name: "Usenet Blackhole".into(),
        description: Some("Writes .nzb files to a folder watched by another client".into()),
        info_url: None,
        fields: tail(f),
    }
}

/// Blackhole "client".
pub struct Blackhole {
    protocol: Protocol,
    http: reqwest::Client,
    settings: BlackholeSettings,
    info: ClientInfo,
}

impl Blackhole {
    pub fn from_settings(
        protocol: Protocol,
        settings: &Value,
        ctx: &BuildContext,
    ) -> Result<Self, ClientError> {
        let (schema, imp) = match protocol {
            Protocol::Torrent => (torrent_schema(), TORRENT_IMPLEMENTATION),
            Protocol::Usenet => (usenet_schema(), USENET_IMPLEMENTATION),
        };
        let s: BlackholeSettings = common::decode_settings(&schema, settings)?;
        Ok(Self {
            protocol,
            http: match protocol {
                Protocol::Torrent => torrent::no_redirect_client(),
                Protocol::Usenet => ctx.http.clone(),
            },
            info: s.common.info(imp),
            settings: s,
        })
    }

    /// Rejects ids that are not a single plain path component.
    fn entry_path(&self, id: &DownloadId) -> Result<PathBuf, ClientError> {
        let s = id.as_str();
        if s.is_empty() || s == "." || s == ".." || s.contains(['/', '\\']) || s.contains('\0') {
            return Err(ClientError::NotFound(s.to_owned()));
        }
        let main = self.settings.watch_folder.join(s);
        if main.exists() {
            return Ok(main);
        }
        for (dir, _) in self.extra_folders() {
            let p = dir.join(s);
            if p.exists() {
                return Ok(p);
            }
        }
        Ok(main)
    }

    /// Configured book/music watch folders with the category they report.
    fn extra_folders(&self) -> Vec<(PathBuf, &'static str)> {
        let mut v: Vec<(PathBuf, &'static str)> = Vec::new();
        for (dir, cat) in [
            (&self.settings.book_folder, BOOK_CATEGORY),
            (&self.settings.music_folder, MUSIC_CATEGORY),
        ] {
            if let Some(d) = dir
                && *d != self.settings.watch_folder
                && !v.iter().any(|e| e.0 == *d)
            {
                v.push((d.clone(), cat));
            }
        }
        v
    }

    async fn write_new(&self, dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> Result<(), ClientError> {
        tokio::fs::create_dir_all(dir).await?;
        // Write to a temp name first so the watcher never sees a partial file.
        let tmp = dir.join(format!(".{stem}{ext}.partial"));
        let dst = dir.join(format!("{stem}{ext}"));
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &dst).await?;
        Ok(())
    }

    fn check_folders(&self) -> Result<(), ClientError> {
        for (label, p) in [
            ("drop", &self.settings.drop_folder),
            ("watch", &self.settings.watch_folder),
        ]
        .into_iter()
        .chain(self.settings.book_folder.as_ref().map(|p| ("book watch", p)))
        .chain(self.settings.music_folder.as_ref().map(|p| ("music watch", p)))
        {
            if !p.is_dir() {
                return Err(ClientError::InvalidSettings(format!(
                    "{label} folder {} does not exist",
                    p.display()
                )));
            }
        }
        Ok(())
    }
}

/// Size and newest modification time of a file or directory tree.
fn scan_entry(path: &Path) -> std::io::Result<(u64, SystemTime)> {
    let meta = std::fs::metadata(path)?;
    let mut newest = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if !meta.is_dir() {
        return Ok((meta.len(), newest));
    }
    let mut size = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let m = e.metadata()?;
            if let Ok(t) = m.modified() {
                newest = newest.max(t);
            }
            if m.is_dir() {
                stack.push(e.path());
            } else {
                size += m.len();
            }
        }
    }
    Ok((size, newest))
}

#[async_trait]
impl DownloadClient for Blackhole {
    fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn info(&self) -> &ClientInfo {
        &self.info
    }

    /// `books`/`music` when the matching watch folder is set; otherwise the main watch folder.
    fn category_for_kind(&self, kind: ContentKind) -> Option<String> {
        let (folder, cat) = match kind {
            ContentKind::Ebook | ContentKind::Audiobook => (&self.settings.book_folder, BOOK_CATEGORY),
            ContentKind::Music => (&self.settings.music_folder, MUSIC_CATEGORY),
            ContentKind::Movie | ContentKind::Series => return None,
        };
        folder.as_ref().map(|_| cat.to_owned())
    }

    async fn add(&self, release: &ReleasePayload, opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let stem = common::sanitize_file_name(if opts.title.trim().is_empty() {
            "release"
        } else {
            opts.title.trim()
        });
        let dir = &self.settings.drop_folder;
        match self.protocol {
            Protocol::Torrent => match torrent::resolve(&self.http, release, &stem).await? {
                TorrentSource::File { bytes, .. } => self.write_new(dir, &stem, ".torrent", &bytes).await?,
                TorrentSource::Magnet { uri, .. } => {
                    if !self.settings.save_magnet_files {
                        return Err(ClientError::InvalidPayload(
                            "magnet links are not supported unless 'Save magnet files' is on".into(),
                        ));
                    }
                    let ext = self.settings.magnet_extension.trim();
                    let ext = if ext.starts_with('.') {
                        ext.to_owned()
                    } else {
                        format!(".{ext}")
                    };
                    self.write_new(dir, &stem, &ext, uri.as_bytes()).await?;
                }
            },
            Protocol::Usenet => {
                let bytes = match release {
                    ReleasePayload::NzbFile { bytes, .. } => bytes.clone(),
                    ReleasePayload::Url(u) => {
                        let resp = self.http.get(u).send().await?;
                        if !resp.status().is_success() {
                            return Err(ClientError::InvalidPayload(format!(
                                "fetching NZB failed: HTTP {}",
                                resp.status()
                            )));
                        }
                        crate::common::read_limited(resp, crate::common::MAX_DOWNLOAD_BYTES).await?
                    }
                    other => {
                        return Err(ClientError::InvalidPayload(format!(
                            "a Usenet blackhole cannot take a {}",
                            other.kind()
                        )));
                    }
                };
                common::validate_nzb(&bytes)?;
                self.write_new(dir, &stem, ".nzb", &bytes).await?;
            }
        }
        Ok(DownloadId(stem))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let mut folders: Vec<(PathBuf, Option<&'static str>)> =
            vec![(self.settings.watch_folder.clone(), None)];
        folders.extend(self.extra_folders().into_iter().map(|(d, c)| (d, Some(c))));
        let settle = Duration::from_secs(self.settings.settle_seconds);
        let read_only = self.settings.read_only && self.protocol == Protocol::Torrent;
        tokio::task::spawn_blocking(move || -> Result<Vec<DownloadItem>, ClientError> {
            let mut out = Vec::new();
            let now = SystemTime::now();
            for (i, (watch, category)) in folders.iter().enumerate() {
                let entries = match std::fs::read_dir(watch) {
                    Ok(r) => r,
                    // A missing extra folder is reported by `status`; it must not hide the main one.
                    Err(e) if i > 0 && e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                for e in entries {
                    let e = e?;
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') || name.ends_with(".partial") || name.starts_with("@eaDir") {
                        continue;
                    }
                    let (size, newest) = scan_entry(&e.path())?;
                    let quiet = now.duration_since(newest).unwrap_or_default() >= settle;
                    let state = if quiet {
                        DownloadState::Completed
                    } else {
                        DownloadState::Downloading
                    };
                    let title = Path::new(&name)
                        .file_stem()
                        .filter(|_| e.path().is_file())
                        .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
                    let mut item = DownloadItem::new(name.clone(), title, state);
                    item.total_bytes = size;
                    item.output_path = Some(e.path().to_string_lossy().into_owned());
                    item.can_be_removed = quiet;
                    item.can_move_files = quiet && !read_only;
                    item.category = category.map(str::to_owned);
                    out.push(item);
                }
            }
            out.sort_by(|a, b| a.download_id.0.cmp(&b.download_id.0));
            Ok(out)
        })
        .await
        .map_err(|e| ClientError::Io(std::io::Error::other(e)))?
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        if !delete_data {
            return Ok(());
        }
        let path = self.entry_path(id)?;
        let meta = match tokio::fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if meta.is_dir() {
            tokio::fs::remove_dir_all(&path).await?;
        } else {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn test(&self) -> Result<(), ClientError> {
        self.check_folders()?;
        // Writable drop folder?
        let probe = self.settings.drop_folder.join(".reelix-write-test");
        tokio::fs::write(&probe, b"ok").await?;
        tokio::fs::remove_file(&probe).await?;
        Ok(())
    }

    async fn test_report(&self) -> TestReport {
        let mut r = TestReport::default();
        match self.test().await {
            Ok(()) => r.pass("folders"),
            Err(e) => r.fail("folders", e.to_string()),
        }
        r
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let mut warnings = Vec::new();
        if let Err(e) = self.check_folders() {
            warnings.push(HealthWarning::new(HealthKind::Misconfigured, e.to_string()));
        }
        Ok(ClientStatus {
            version: None,
            output_dirs: std::iter::once(OutputDir {
                category: None,
                path: self.settings.watch_folder.to_string_lossy().into_owned(),
            })
            .chain(self.extra_folders().into_iter().map(|(d, c)| OutputDir {
                category: Some(c.to_owned()),
                path: d.to_string_lossy().into_owned(),
            }))
            .collect(),
            warnings,
        })
    }
}
