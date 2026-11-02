//! Neutral rendering of an event (title, body, key/value fields) that each notifier
//! formats for its own service. Keeps wording consistent across services.

use crate::event::{HealthLevel, MediaSummary, NotificationEvent, ReleaseSummary};

/// Coarse severity; maps to colours, priorities and Apprise/ntfy types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Failure,
}

impl Severity {
    /// RGB colour used by Discord/Slack style embeds.
    pub fn color(self) -> u32 {
        match self {
            Self::Info => 0x3B82F6,
            Self::Success => 0x22C55E,
            Self::Warning => 0xF59E0B,
            Self::Failure => 0xEF4444,
        }
    }

    /// `info|success|warning|failure` (Apprise vocabulary).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Failure => "failure",
        }
    }
}

/// A labelled value shown under the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub value: String,
}

/// Service-agnostic rendering of a [`NotificationEvent`].
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Short headline, e.g. `Grabbed: Dune (2021)`.
    pub title: String,
    /// Main text (plain, unescaped).
    pub body: String,
    pub fields: Vec<Field>,
    /// Link to more info (TMDB/IMDb page, release notes, wiki).
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub severity: Severity,
    /// The media the event is about (`Dune (2021)`); empty for events without media.
    pub subject: String,
    /// What happened (`Imported`, `Grabbed`…), the title without the subject.
    pub headline: String,
    /// Media synopsis, for services that show one (Discord).
    pub overview: Option<String>,
    /// Labelled links (TMDb, IMDb, Trakt, TVDB, YouTube).
    pub links: Vec<(String, String)>,
    /// Release (scene) name of a grab or import.
    pub release: Option<String>,
    /// Wide backdrop image.
    pub backdrop_url: Option<String>,
}

impl Message {
    /// Renders an event. All strings are raw; escape per target format.
    pub fn from_event(event: &NotificationEvent) -> Self {
        let media = event.media();
        let subject = media.map(|m| m.display_title()).unwrap_or_default();
        let mut m = Message {
            title: String::new(),
            body: String::new(),
            fields: Vec::new(),
            url: media.and_then(|m| m.link()),
            image_url: media.and_then(|m| m.poster_url.clone()),
            severity: Severity::Info,
            subject: subject.clone(),
            headline: String::new(),
            overview: None,
            links: Vec::new(),
            release: None,
            backdrop_url: None,
        };
        let head = |h: &str| {
            if subject.is_empty() {
                h.to_string()
            } else {
                format!("{h}: {subject}")
            }
        };
        match event {
            NotificationEvent::Test { message } => {
                m.title = "Reelix test notification".into();
                m.body = message.clone();
            }
            NotificationEvent::Grab { release, .. } => {
                m.title = head("Grabbed");
                m.body = release.title.clone();
                m.release = Some(release.title.clone());
                media_fields(&mut m, media);
                release_fields(&mut m, release);
            }
            NotificationEvent::Import {
                release,
                file,
                is_upgrade,
                replaced_files,
                ..
            } => {
                m.title = head(if *is_upgrade { "Upgraded" } else { "Imported" });
                m.severity = Severity::Success;
                m.body = file.path.clone();
                m.release = file
                    .scene_name
                    .clone()
                    .or_else(|| release.as_ref().map(|r| r.title.clone()));
                media_fields(&mut m, media);
                m.push_opt("Quality", file.quality.clone());
                let codecs: Vec<String> = [file.video_codec.clone(), file.audio_codec.clone()]
                    .into_iter()
                    .flatten()
                    .collect();
                m.push("Codecs", codecs.join(" / "));
                m.push_opt("Size", file.size_bytes.map(format_size));
                m.push("Languages", file.audio_languages.join(", "));
                m.push("Subtitles", file.subtitle_languages.join(", "));
                m.push_opt("Release group", file.release_group.clone());
                if let Some(r) = release {
                    m.push_opt("Indexer", r.indexer.clone());
                    m.push_opt("Download client", r.download_client.clone());
                }
                if !replaced_files.is_empty() {
                    let old: Vec<_> = replaced_files
                        .iter()
                        .map(|f| f.quality.clone().unwrap_or_else(|| f.path.clone()))
                        .collect();
                    m.push("Replaced", old.join(", "));
                }
            }
            NotificationEvent::Rename { files, .. } => {
                m.title = head("Renamed");
                m.body = format!("{} file(s) renamed", files.len());
                for f in files.iter().take(5) {
                    m.push(&f.previous_path, f.new_path.clone());
                }
            }
            NotificationEvent::MediaAdded { media } => {
                m.title = head("Added");
                m.body = media.overview.clone().unwrap_or_default();
            }
            NotificationEvent::MediaDeleted { files_deleted, .. } => {
                m.title = head("Deleted");
                m.severity = Severity::Warning;
                m.body = if *files_deleted {
                    "Removed from the library and files deleted".into()
                } else {
                    "Removed from the library (files kept)".into()
                };
            }
            NotificationEvent::FileDeleted { file, reason, .. } => {
                m.title = head("File deleted");
                m.severity = Severity::Warning;
                m.body = file.path.clone();
                m.push("Reason", format!("{reason:?}"));
                m.push_opt("Quality", file.quality.clone());
            }
            NotificationEvent::HealthIssue(h) => {
                m.title = "Health issue".into();
                m.severity = match h.level {
                    HealthLevel::Error => Severity::Failure,
                    _ => Severity::Warning,
                };
                m.body = h.message.clone();
                m.push("Source", h.source.clone());
                m.url = h.wiki_url.clone();
            }
            NotificationEvent::HealthRestored(h) => {
                m.title = "Health issue resolved".into();
                m.severity = Severity::Success;
                m.body = h.message.clone();
                m.push("Source", h.source.clone());
            }
            NotificationEvent::ManualInteractionRequired { release, message, .. } => {
                m.title = head("Manual interaction required");
                m.severity = Severity::Warning;
                m.body = message.clone();
                m.push("Release", release.title.clone());
                release_fields(&mut m, release);
            }
            NotificationEvent::RequestPending { request, .. }
            | NotificationEvent::RequestApproved { request, .. }
            | NotificationEvent::RequestDeclined { request, .. }
            | NotificationEvent::RequestAvailable { request, .. } => {
                let (h, sev) = match event {
                    NotificationEvent::RequestPending { .. } => {
                        ("New request pending approval", Severity::Info)
                    }
                    NotificationEvent::RequestApproved { .. } => ("Request approved", Severity::Success),
                    NotificationEvent::RequestDeclined { .. } => ("Request declined", Severity::Failure),
                    _ => ("Now available", Severity::Success),
                };
                m.title = head(h);
                m.severity = sev;
                m.body = media.and_then(|x| x.overview.clone()).unwrap_or_default();
                m.push("Requested by", request.requested_by.clone());
                if !request.seasons.is_empty() {
                    let s: Vec<String> = request.seasons.iter().map(u32::to_string).collect();
                    m.push("Seasons", s.join(", "));
                }
                if request.is_4k {
                    m.push("4K", "Yes".into());
                }
                m.push_opt("By", request.modified_by.clone());
                m.push_opt("Comment", request.comment.clone());
            }
            NotificationEvent::IssueReported { issue, .. } => {
                m.title = head(&format!("New {} issue", issue.issue_type));
                m.severity = Severity::Warning;
                m.body = issue.message.clone();
                m.push("Reported by", issue.reported_by.clone());
            }
            NotificationEvent::IssueResolved { issue, .. } => {
                m.title = head(&format!("{} issue resolved", issue.issue_type));
                m.severity = Severity::Success;
                m.body = issue.message.clone();
                m.push("Reported by", issue.reported_by.clone());
                m.push_opt("Resolved by", issue.resolved_by.clone());
                m.push_opt("Comment", issue.comment.clone());
            }
            NotificationEvent::ApplicationUpdate(u) => {
                m.title = "Reelix updated".into();
                m.body = format!("{} → {}", u.previous_version, u.new_version);
                if !u.changes.is_empty() {
                    m.push("Changes", u.changes.join("\n"));
                }
                m.url = u.release_notes_url.clone();
            }
        }
        if let Some(x) = media {
            m.headline = match m.title.strip_suffix(&format!(": {subject}")) {
                Some(h) if !subject.is_empty() => h.to_owned(),
                _ => m.title.clone(),
            };
            m.overview = x.overview.clone().filter(|o| !o.trim().is_empty());
            m.links = x
                .links()
                .into_iter()
                .map(|(name, url)| (name.to_owned(), url))
                .collect();
            m.backdrop_url = x.backdrop_url.clone();
        }
        // Books and music: say what was grabbed / imported first.
        if matches!(
            event,
            NotificationEvent::Grab { .. }
                | NotificationEvent::Import { .. }
                | NotificationEvent::ManualInteractionRequired { .. }
        ) && let Some(label) = media.and_then(|x| x.content_label())
        {
            m.fields.insert(
                0,
                Field {
                    name: "Type".into(),
                    value: label.into(),
                },
            );
        }
        m
    }

    fn push(&mut self, name: &str, value: String) {
        if !value.is_empty() {
            self.fields.push(Field {
                name: name.into(),
                value,
            });
        }
    }

    fn push_opt(&mut self, name: &str, value: Option<String>) {
        if let Some(v) = value {
            self.push(name, v);
        }
    }

    /// Body followed by `Name: value` lines (no title).
    pub fn text(&self) -> String {
        let mut out = self.body.clone();
        for f in &self.fields {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&f.name);
            out.push_str(": ");
            out.push_str(&f.value);
        }
        if out.is_empty() {
            out = self.title.clone();
        }
        out
    }

    /// HTML fragment (escaped) of body + fields, with the title in bold when `with_title`.
    pub fn html(&self, with_title: bool) -> String {
        let mut out = String::new();
        if with_title {
            out.push_str("<b>");
            out.push_str(&escape_html(&self.title));
            out.push_str("</b>\n");
        }
        if !self.body.is_empty() {
            out.push_str(&escape_html(&self.body));
            out.push('\n');
        }
        for f in &self.fields {
            out.push_str("<b>");
            out.push_str(&escape_html(&f.name));
            out.push_str(":</b> ");
            out.push_str(&escape_html(&f.value));
            out.push('\n');
        }
        if let Some(u) = &self.url {
            out.push_str("<a href=\"");
            out.push_str(&escape_html(u));
            out.push_str("\">More info</a>");
        }
        out.trim_end().to_string()
    }

    /// Markdown body + fields (used by Gotify/ntfy/Apprise markdown modes).
    pub fn markdown(&self) -> String {
        let mut out = escape_markdown(&self.body);
        for f in &self.fields {
            if !out.is_empty() {
                out.push_str("  \n");
            }
            out.push_str(&format!(
                "**{}:** {}",
                escape_markdown(&f.name),
                escape_markdown(&f.value)
            ));
        }
        out
    }
}

/// Rating and genres of the media.
fn media_fields(m: &mut Message, media: Option<&MediaSummary>) {
    let Some(media) = media else { return };
    m.push_opt(
        "Rating",
        media.rating.filter(|r| *r > 0.0).map(|r| format!("{r:.1}")),
    );
    m.push(
        "Genres",
        media
            .genres
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    );
}

fn release_fields(m: &mut Message, r: &ReleaseSummary) {
    m.push_opt("Quality", r.quality.clone());
    m.push_opt("Size", r.size_bytes.map(format_size));
    m.push_opt("Indexer", r.indexer.clone());
    m.push_opt("Download client", r.download_client.clone());
    m.push_opt("Release group", r.release_group.clone());
    if !r.custom_formats.is_empty() {
        let score = r
            .custom_format_score
            .map(|s| format!(" ({s:+})"))
            .unwrap_or_default();
        m.push(
            "Custom formats",
            format!("{}{score}", r.custom_formats.join(", ")),
        );
    }
}

/// Human-readable size with binary units (`1.5 GiB`).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Escapes `& < > "` for HTML text and attributes.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Backslash-escapes Markdown control characters.
pub fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use reelix_core::MediaKind;

    #[test]
    fn grab_message_has_fields_and_escapes_html() {
        let mut media = MediaSummary::new(MediaKind::Movie, "Tom & Jerry <Remastered>");
        media.year = Some(1999);
        let ev = NotificationEvent::Grab {
            media,
            release: ReleaseSummary {
                title: "Tom.and.Jerry.1999.1080p.BluRay.x264-GRP".into(),
                quality: Some("Bluray-1080p".into()),
                size_bytes: Some(8 * 1024 * 1024 * 1024),
                custom_formats: vec!["x264".into()],
                custom_format_score: Some(10),
                ..Default::default()
            },
        };
        let m = Message::from_event(&ev);
        assert_eq!(m.title, "Grabbed: Tom & Jerry <Remastered> (1999)");
        assert!(m.text().contains("Size: 8.0 GiB"));
        assert!(m.text().contains("Custom formats: x264 (+10)"));
        let html = m.html(true);
        assert!(html.starts_with("<b>Grabbed: Tom &amp; Jerry &lt;Remastered&gt; (1999)</b>"));
        assert!(escape_markdown("a*b_c").contains("a\\*b\\_c"));
    }

    #[test]
    fn book_and_album_messages_name_the_content() {
        use reelix_core::ContentKind;
        let mut book = MediaSummary::content(ContentKind::Audiobook, "Frank Herbert - Dune");
        book.year = Some(1965);
        book.tmdb_id = Some(1);
        let m = Message::from_event(&NotificationEvent::Grab {
            media: book,
            release: ReleaseSummary {
                title: "Frank Herbert - Dune (1965) [M4B]".into(),
                quality: Some("M4B".into()),
                ..Default::default()
            },
        });
        assert_eq!(m.title, "Grabbed: Frank Herbert - Dune (1965)");
        assert_eq!(m.fields[0].value, "Audiobook");
        assert!(m.url.is_none(), "no TMDB link for books");
        let album = MediaSummary::content(ContentKind::Music, "Radiohead - OK Computer");
        let m = Message::from_event(&NotificationEvent::Import {
            media: album,
            release: None,
            file: FileSummary {
                path: "/music/Radiohead/OK Computer (1997)".into(),
                quality: Some("FLAC".into()),
                ..Default::default()
            },
            is_upgrade: true,
            replaced_files: vec![],
        });
        assert_eq!(m.title, "Upgraded: Radiohead - OK Computer");
        assert!(m.text().contains("Type: Album"));
        assert!(m.text().contains("Quality: FLAC"));
    }

    #[test]
    fn every_event_renders_a_title() {
        for ev in crate::samples::all() {
            let m = Message::from_event(&ev);
            assert!(!m.title.is_empty(), "{:?}", ev.kind());
            assert!(!m.text().is_empty());
        }
    }
}
