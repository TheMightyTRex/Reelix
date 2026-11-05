//! Completed book, album and radio downloads: handed to the books / music importers
//! ([`reelix_books::BooksService::import_api`], [`reelix_music::MusicService::import_api`]) or
//! filed into the podcast library ([`reelix_radio::RadioService::file_download`]) and recorded
//! like movie/episode imports (history, notification, media server refresh).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use reelix_core::ContentKind;
use reelix_downloaders::DownloadItem;
use reelix_import::files::TransferMode;
use reelix_notify::{FileSummary, MediaSummary, NotificationEvent};
use reelix_parser::QualityModel;

use super::{
    ActivityService, Busy, ClientHandle, ContentDownload, ContentRef, DownloadRef, Result, fill_release,
    lock, release_summary,
};
use crate::store::history::{self, HistoryEvent, NewHistory};
use crate::store::tracked::{self, TrackedDownload, TrackedState};

/// One imported file, for history and notifications.
struct ImportedContent {
    destination: PathBuf,
    source: PathBuf,
    size: u64,
    quality: QualityModel,
    release_group: Option<String>,
}

/// What a content import produced.
enum Outcome {
    Imported {
        files: Vec<ImportedContent>,
        is_upgrade: bool,
        replaced: Vec<PathBuf>,
    },
    /// Not imported; the queue item waits with these messages.
    Pending(Vec<String>),
    Rejected(Vec<String>),
}

impl ActivityService {
    /// Author / artist ids of a content reference (best effort, for history rows).
    pub(crate) async fn content_owner(&self, c: &ContentRef) -> (Option<i64>, Option<i64>) {
        if let (Some(id), Some(b)) = (c.book_id, self.books.get())
            && let Ok(book) = b.get_book(id).await
        {
            return (Some(book.author_id), None);
        }
        if let (Some(id), Some(m)) = (c.album_id, self.music.get())
            && let Ok(album) = m.get_album(id).await
        {
            return (None, Some(album.artist_id));
        }
        (None, None)
    }

    /// Sets the content columns (and owner ids) of a history row.
    pub(crate) async fn fill_content(&self, h: &mut NewHistory, c: &ContentRef) {
        if c.is_empty() {
            return;
        }
        h.content = *c;
        let (author, artist) = self.content_owner(c).await;
        h.author_id = author;
        h.artist_id = artist;
    }

    /// Notification summary of a book format (`Author - Title`), album (`Artist - Album`) or
    /// radio episode (`Show - S20E03 - Title`).
    pub(crate) async fn content_summary(&self, c: &ContentRef) -> Option<MediaSummary> {
        if let Some(id) = c.radio_episode_id {
            let r = self.radio.get()?;
            let e = r.get_episode(id).await.ok()?;
            let show = r.get_show(e.show_id).await.ok()?;
            let series = reelix_radio::status::filing_series_number(e.series_kind, e.series_number);
            let mut title = show.title.clone();
            if let Some(n) = e.number {
                title.push_str(&format!(" - S{series:02}E{n:02}"));
            }
            if !e.title.trim().is_empty() {
                title.push_str(&format!(" - {}", e.title.trim()));
            }
            let mut s = MediaSummary::content(ContentKind::Radio, title);
            s.id = Some(e.id);
            s.year = e.first_broadcast.map(|d| chrono::Datelike::year(&d));
            s.overview = e.synopsis;
            return Some(s);
        }
        if let Some((id, format)) = c.book_ref() {
            let b = self.books.get()?.get_book(id).await.ok()?;
            let mut s =
                MediaSummary::content(format.content_kind(), format!("{} - {}", b.author_name, b.title));
            s.id = Some(b.id);
            s.year = b.year;
            s.poster_url = b.images.poster;
            s.overview = b.overview;
            return Some(s);
        }
        let id = c.album_id?;
        let a = self.music.get()?.get_album(id).await.ok()?;
        let mut s = MediaSummary::content(ContentKind::Music, format!("{} - {}", a.artist_name, a.title));
        s.id = Some(a.id);
        s.year = a.release_date.map(|d| chrono::Datelike::year(&d));
        s.poster_url = a.images.poster;
        Some(s)
    }

    /// Enqueues a replacement search for a book format / album / radio episode (errors logged).
    pub(crate) async fn search_again_content(&self, c: &ContentRef) {
        let r = if let Some(id) = c.radio_episode_id {
            match self.radio.get() {
                Some(r) => r
                    .request_search(reelix_api_types::radio::RadioSearchRequest {
                        kind: reelix_api_types::content::ContentSearchKind::Item,
                        ids: vec![id],
                    })
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                None => return,
            }
        } else if let Some((id, format)) = c.book_ref() {
            match self.books.get() {
                Some(b) => b
                    .request_search(reelix_api_types::books::BookSearchRequest {
                        kind: reelix_api_types::content::ContentSearchKind::Item,
                        ids: vec![id],
                        format: Some(format),
                    })
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                None => return,
            }
        } else if let Some(id) = c.album_id {
            match self.music.get() {
                Some(m) => m
                    .request_search(reelix_api_types::music::AlbumSearchRequest {
                        kind: reelix_api_types::content::ContentSearchKind::Item,
                        ids: vec![id],
                    })
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                None => return,
            }
        } else {
            return;
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "could not queue a replacement search");
        }
    }

    async fn run_content_import(
        &self,
        t: &TrackedDownload,
        d: &ContentDownload,
    ) -> std::result::Result<Outcome, String> {
        if let Some(id) = t.content.radio_episode_id {
            let radio = self.radio.get().ok_or("radio is not available")?;
            let out = radio.file_download(d, id).await.map_err(|e| e.to_string())?;
            return Ok(match out {
                reelix_radio::FilingOutcome::Filed(f) => Outcome::Imported {
                    files: vec![ImportedContent {
                        destination: f.destination,
                        source: f.source,
                        size: f.size,
                        quality: f.quality,
                        release_group: f.release_group,
                    }],
                    is_upgrade: false,
                    replaced: Vec::new(),
                },
                reelix_radio::FilingOutcome::Rejected(msgs) => Outcome::Rejected(if msgs.is_empty() {
                    vec!["No audio file could be filed".to_owned()]
                } else {
                    msgs
                }),
            });
        }
        if let Some((id, format)) = t.content.book_ref() {
            let books = self.books.get().ok_or("books are not available")?;
            let out = books
                .import_api()
                .import_download(d, id, format)
                .await
                .map_err(|e| e.to_string())?;
            if out.imported.is_empty() {
                let msgs = if out.rejections.is_empty() {
                    vec![format!("No {} files could be imported", format.as_str())]
                } else {
                    out.rejections
                };
                return Ok(Outcome::Rejected(msgs));
            }
            let is_upgrade = out.imported.iter().any(|f| f.is_upgrade);
            let replaced = out.imported.iter().flat_map(|f| f.replaced.clone()).collect();
            let files = out
                .imported
                .into_iter()
                .map(|f| ImportedContent {
                    destination: f.destination,
                    source: f.source,
                    size: f.size,
                    quality: f.quality,
                    release_group: f.release_group,
                })
                .collect();
            return Ok(Outcome::Imported {
                files,
                is_upgrade,
                replaced,
            });
        }
        let id = t.content.album_id.ok_or("no book or album")?;
        let music = self.music.get().ok_or("music is not available")?;
        let out = music
            .import_api()
            .import_download(d, id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(match out {
            reelix_music::AlbumImportOutcome::Imported(a) => {
                let group = reelix_parser::parse_music(&t.title).release_group;
                Outcome::Imported {
                    files: a
                        .files
                        .into_iter()
                        .map(|f| ImportedContent {
                            destination: f.destination,
                            source: f.source,
                            size: f.size,
                            quality: f.quality,
                            release_group: group.clone(),
                        })
                        .collect(),
                    is_upgrade: a.is_upgrade,
                    replaced: a.replaced,
                }
            }
            reelix_music::AlbumImportOutcome::NeedsRelease { candidates, .. } => {
                let names: Vec<String> = candidates
                    .iter()
                    .take(3)
                    .map(|c| format!("{} ({} tracks)", c.title, c.track_count))
                    .collect();
                let mut msg = format!(
                    "Several releases of the album match the files ({} candidates); choose the release on the album page, then retry the import",
                    candidates.len()
                );
                if !names.is_empty() {
                    msg.push_str(&format!(" ({})", names.join(", ")));
                }
                Outcome::Pending(vec![msg])
            }
            reelix_music::AlbumImportOutcome::Rejected(msgs) => Outcome::Rejected(if msgs.is_empty() {
                vec!["No audio files could be imported".to_owned()]
            } else {
                msgs
            }),
        })
    }

    /// Imports a completed book / album download. Returns whether it was imported.
    pub(crate) async fn import_content(
        &self,
        t: &TrackedDownload,
        source: &Path,
        mode: TransferMode,
        client: Option<(&ClientHandle, &DownloadItem)>,
    ) -> Result<bool> {
        if !lock(&self.busy).insert(t.id) {
            return Ok(false);
        }
        let _busy = Busy(&self.busy, t.id);
        tracked::set_state(&self.db, t.id, TrackedState::Importing, &[]).await?;
        let mut current = t.clone();
        current.state = TrackedState::Importing;
        current.status_messages.clear();
        self.emit("queue", "updated", t.id);
        let r = self.download_ref(t);
        let d = ContentDownload {
            source: source.to_path_buf(),
            release_title: Some(t.title.clone()),
            mode,
            download_client: r.client.clone(),
            download_id: Some(t.download_id.clone()),
        };
        let imported = match self.run_content_import(t, &d).await {
            Ok(Outcome::Imported {
                files,
                is_upgrade,
                replaced,
            }) => {
                self.record_content_import(t, &r, &files, is_upgrade, &replaced)
                    .await;
                // Media servers are refreshed from the import's change event by the server's
                // library refresher, which only asks servers that serve books/music.
                self.set_state(&current, TrackedState::Imported, Vec::new())
                    .await?;
                if let Some((h, item)) = client
                    && h.client.info().remove_completed
                    && item.can_be_removed
                {
                    self.remove_from_client(h, &item.download_id, true).await;
                }
                true
            }
            Ok(Outcome::Pending(msgs)) => {
                self.needs_manual(&current, msgs).await?;
                false
            }
            Ok(Outcome::Rejected(msgs)) => {
                let mut h = NewHistory::new(HistoryEvent::ImportFailed, &t.title);
                self.fill_content(&mut h, &t.content).await;
                fill_release(&mut h, &r);
                h.data = serde_json::json!({ "messages": msgs, "source_path": source });
                history::insert(&self.db, &h).await?;
                self.emit("history", "created", 0);
                self.needs_manual(&current, msgs).await?;
                false
            }
            Err(e) => {
                tracing::warn!(id = t.id, error = %e, "content import failed");
                self.needs_manual(&current, vec![format!("Import failed: {e}")])
                    .await?;
                false
            }
        };
        self.dirty.store(true, Ordering::Relaxed);
        Ok(imported)
    }

    /// History row (one per download), import notification.
    async fn record_content_import(
        &self,
        t: &TrackedDownload,
        r: &DownloadRef,
        files: &[ImportedContent],
        is_upgrade: bool,
        replaced: &[PathBuf],
    ) {
        let Some(first) = files.first() else { return };
        let size: u64 = files.iter().map(|f| f.size).sum();
        let mut h = NewHistory::new(HistoryEvent::Imported, &r.title);
        self.fill_content(&mut h, &t.content).await;
        fill_release(&mut h, r);
        h.quality = Some(first.quality);
        h.data = serde_json::json!({
            "source_path": first.source,
            "destination_path": first.destination,
            "files": files.iter().map(|f| f.destination.clone()).collect::<Vec<_>>(),
            "size": size,
            "is_upgrade": is_upgrade,
            "replaced": replaced,
            "release_group": first.release_group,
        });
        if let Err(e) = history::insert(&self.db, &h).await {
            tracing::warn!(error = %e, "could not write import history");
        }
        self.emit("history", "created", 0);
        let Some(media) = self.content_summary(&t.content).await else {
            return;
        };
        let file = FileSummary {
            path: if files.len() > 1 {
                first
                    .destination
                    .parent()
                    .unwrap_or(&first.destination)
                    .to_string_lossy()
                    .into_owned()
            } else {
                first.destination.to_string_lossy().into_owned()
            },
            size_bytes: Some(size),
            quality: Some(first.quality.quality.name().to_owned()),
            release_group: first.release_group.clone(),
            scene_name: Some(t.title.clone()),
            ..Default::default()
        };
        let replaced_files = replaced
            .iter()
            .map(|p| FileSummary {
                path: p.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .collect();
        self.providers
            .notify(&NotificationEvent::Import {
                media,
                release: Some(release_summary(r)),
                file,
                is_upgrade,
                replaced_files,
            })
            .await;
    }
}
