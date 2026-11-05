//! Book, album and radio show requests (M8/M9, radio Discover): create (permissions,
//! duplicates, availability, quotas, auto-approve), approve / decline / retry / edit / delete
//! and the status sync. An approved radio request follows the show (the radio settings'
//! default follow choice).
//!
//! They share the `requests` table, statuses, notifications and request settings with movie/TV
//! requests but use their own DTOs (`/requests/media`), so movie/TV JSON stays unchanged.

use std::collections::{BTreeSet, HashMap};

use reelix_api_types::requests::{
    AvailabilityStatus, ContentRequestBriefDto, ContentRequestDto, ContentRequestKind, ContentRequestPageDto,
    CreateContentRequestDto, NamedIdDto, RequestDefaultsDto, RequestListQuery, RequestOptionsDto,
    RequestSettingsDto, RequestStatus, UpdateContentRequestDto, UserBriefDto,
};
use reelix_core::{BookFormat, ContentKind, Permissions};
use reelix_db::now_ms;
use reelix_notify::{MediaSummary, NotificationEvent, RequestSummary};

use reelix_api_types::radio::RadioShowPreviewDto;
use reelix_api_types::requests::split_radio_foreign_id;

use crate::content_repo::{ContentFilter, ContentRepo, ContentRow, NewContent};
use crate::gateway::{AddAlbumRequest, AddBookRequest, AddRadioRequest, Availability, ContentHit};
use crate::repo::ts;
use crate::requests::{
    NotifyKind, movie_status, notify_status, require_manager, sees_all_requests, status_filter, unknown_user,
};
use crate::{Actor, Error, RequestService, Result};

pub(crate) fn content_permission(kind: ContentRequestKind) -> Permissions {
    match kind {
        ContentRequestKind::Book => Permissions::REQUEST_BOOK,
        ContentRequestKind::Music => Permissions::REQUEST_MUSIC,
        ContentRequestKind::Radio => Permissions::REQUEST_RADIO,
    }
}

fn auto_approve_permission(kind: ContentRequestKind) -> Permissions {
    match kind {
        ContentRequestKind::Book => Permissions::AUTO_APPROVE_BOOK,
        ContentRequestKind::Music => Permissions::AUTO_APPROVE_MUSIC,
        ContentRequestKind::Radio => Permissions::AUTO_APPROVE_RADIO,
    }
}

/// `book` / `album` / `show`, for messages.
pub(crate) fn kind_word(kind: ContentRequestKind) -> &'static str {
    match kind {
        ContentRequestKind::Book => "book",
        ContentRequestKind::Music => "album",
        ContentRequestKind::Radio => "show",
    }
}

/// "You are not allowed to request …" (lower case for errors, capitalised for the UI).
pub(crate) fn not_allowed(kind: ContentRequestKind) -> &'static str {
    match kind {
        ContentRequestKind::Book => "you are not allowed to request books",
        ContentRequestKind::Music => "you are not allowed to request music",
        ContentRequestKind::Radio => "you are not allowed to request radio shows",
    }
}

/// Whether `actor` may use the advanced request options.
pub(crate) fn advanced_allowed(actor: &Actor) -> bool {
    actor.permissions.has(Permissions::REQUEST_ADVANCED)
        || actor.permissions.has(Permissions::MANAGE_REQUESTS)
}

/// Sorted, de-duplicated formats.
fn normalize_formats(formats: &[BookFormat]) -> Vec<BookFormat> {
    formats
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Formats a new book request gets when it names none: the configured defaults, else both.
pub(crate) fn default_formats(s: &RequestSettingsDto) -> Vec<BookFormat> {
    let d = normalize_formats(&s.book_default_formats);
    if d.is_empty() { BookFormat::ALL.to_vec() } else { d }
}

pub(crate) fn content_defaults(s: &RequestSettingsDto, kind: ContentKind) -> &RequestDefaultsDto {
    match kind {
        ContentKind::Audiobook => &s.book_audiobook,
        ContentKind::Music => &s.music,
        ContentKind::Radio => &s.radio,
        _ => &s.book_ebook,
    }
}

fn format_kind(f: BookFormat) -> ContentKind {
    match f {
        BookFormat::Ebook => ContentKind::Ebook,
        BookFormat::Audiobook => ContentKind::Audiobook,
    }
}

/// Formats covered by the given (active) requests.
pub(crate) fn requested_formats<'a>(rows: impl IntoIterator<Item = &'a ContentRow>) -> BTreeSet<BookFormat> {
    rows.into_iter()
        .flat_map(|r| {
            if r.formats.is_empty() {
                BookFormat::ALL.to_vec()
            } else {
                r.formats.clone()
            }
        })
        .collect()
}

pub(crate) fn to_brief(r: &ContentRow, briefs: &HashMap<i64, UserBriefDto>) -> ContentRequestBriefDto {
    ContentRequestBriefDto {
        id: r.id,
        status: r.status,
        formats: r.formats.clone(),
        requested_by: briefs
            .get(&r.user_id)
            .cloned()
            .unwrap_or_else(|| unknown_user(r.user_id)),
        created_at: ts(r.created_at),
    }
}

/// Radio ids and search terms that are links make the server read them (a feed, a page). Users
/// without MANAGE_LIBRARY (who could already preview any link on the radio pages) may only
/// have public http(s) hosts read: no IP literals or names that are, or resolve to, loopback,
/// private, link-local, shared or unspecified addresses, and no single-label or `.local` /
/// `.internal` / `.localhost` names.
pub(crate) async fn check_radio_link(actor: &Actor, s: &str) -> Result<()> {
    let s = s.trim();
    if !s.contains("://") || actor.permissions.has(Permissions::MANAGE_LIBRARY) {
        return Ok(());
    }
    if public_link(s).await {
        Ok(())
    } else {
        Err(Error::Validation(
            "this link cannot be read; search by name, or ask a library manager to add the show".into(),
        ))
    }
}

async fn public_link(s: &str) -> bool {
    let Ok(u) = url::Url::parse(s) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let port = u.port_or_known_default().unwrap_or(443);
    match u.host() {
        Some(url::Host::Ipv4(ip)) => public_ip(ip.into()),
        Some(url::Host::Ipv6(ip)) => public_ip(ip.into()),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if !d.contains('.')
                || [".local", ".internal", ".localhost", ".lan", ".home.arpa"]
                    .iter()
                    .any(|t| d.ends_with(t))
            {
                return false;
            }
            match tokio::net::lookup_host((d.as_str(), port)).await {
                Ok(addrs) => {
                    let addrs: Vec<_> = addrs.collect();
                    !addrs.is_empty() && addrs.iter().all(|a| public_ip(a.ip()))
                }
                Err(_) => false,
            }
        }
        None => false,
    }
}

fn public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64/10 (shared address space), 0/8.
                || (a == 100 && (64..128).contains(&b))
                || a == 0)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_ip(IpAddr::V4(v4)),
            None => {
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local())
            }
        },
    }
}

fn to_dto(r: ContentRow, briefs: &HashMap<i64, UserBriefDto>) -> ContentRequestDto {
    ContentRequestDto {
        id: r.id,
        kind: r.kind,
        foreign_id: r.foreign_id,
        title: r.title,
        subtitle: r.subtitle,
        year: r.year,
        poster: r.poster,
        formats: r.formats,
        status: r.status,
        profile_id: r.profile_id,
        root_folder_id: r.root_folder_id,
        tags: r.tags,
        requested_by: briefs
            .get(&r.user_id)
            .cloned()
            .unwrap_or_else(|| unknown_user(r.user_id)),
        modified_by: r
            .modified_by
            .map(|m| briefs.get(&m).cloned().unwrap_or_else(|| unknown_user(m))),
        decline_reason: r.decline_reason,
        failure_reason: r.failure_reason,
        book_id: r.book_id,
        author_id: r.author_id,
        album_id: r.album_id,
        artist_id: r.artist_id,
        radio_show_id: r.radio_show_id,
        created_at: ts(r.created_at),
        updated_at: ts(r.updated_at),
        approved_at: r.approved_at.map(ts),
        available_at: r.available_at.map(ts),
    }
}

impl RequestService {
    pub(crate) fn content_repo(&self) -> ContentRepo<'_> {
        ContentRepo(&self.db)
    }

    /// The item behind `id`: a work / release group from the metadata lookup, or for radio
    /// the show's preview (its source's quick read, newest episodes included). `None` = unknown.
    /// A radio hit keeps the requested id as `foreign_id`. A radio id that is a link (a feed)
    /// is read for users without MANAGE_LIBRARY only when it points at a public host.
    pub(crate) async fn lookup_content(
        &self,
        actor: &Actor,
        kind: ContentRequestKind,
        id: &str,
    ) -> Result<Option<(ContentHit, Option<RadioShowPreviewDto>)>> {
        let upstream = |e: anyhow::Error| Error::Upstream(format!("metadata: {e}"));
        if kind != ContentRequestKind::Radio {
            return Ok(self
                .library
                .content(kind, id)
                .await
                .map_err(upstream)?
                .map(|h| (h, None)));
        }
        let Some((provider, external_id)) = split_radio_foreign_id(id) else {
            return Ok(None);
        };
        check_radio_link(actor, external_id).await?;
        let Some(preview) = self
            .library
            .radio_preview(provider, external_id)
            .await
            .map_err(upstream)?
        else {
            return Ok(None);
        };
        let mut hit = ContentHit::radio(&preview.show);
        hit.foreign_id = id.to_owned();
        hit.year = preview
            .first_broadcast
            .map(|t| chrono::Datelike::year(&t.date_naive()));
        Ok(Some((hit, Some(preview))))
    }

    pub async fn content_request_dtos(&self, rows: Vec<ContentRow>) -> Result<Vec<ContentRequestDto>> {
        let ids = rows
            .iter()
            .flat_map(|r| std::iter::once(r.user_id).chain(r.modified_by));
        let briefs = self.users_repo().briefs(ids).await?;
        Ok(rows.into_iter().map(|r| to_dto(r, &briefs)).collect())
    }

    async fn content_request_dto(&self, row: ContentRow) -> Result<ContentRequestDto> {
        Ok(self.content_request_dtos(vec![row]).await?.remove(0))
    }

    async fn load_content(&self, id: i64) -> Result<ContentRow> {
        self.content_repo()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("request not found".into()))
    }

    /// One book/album request; requesters may only read their own.
    pub async fn get_content_request(&self, actor: &Actor, id: i64) -> Result<ContentRequestDto> {
        let row = self.load_content(id).await?;
        if row.user_id != actor.id && !sees_all_requests(actor.permissions) {
            return Err(Error::NotFound("request not found".into()));
        }
        self.content_request_dto(row).await
    }

    /// Book/album/radio requests (own, or all with VIEW_REQUESTS). `kind`: `book`, `music`,
    /// `radio` or `all`.
    pub async fn list_content_requests(
        &self,
        actor: &Actor,
        q: &RequestListQuery,
    ) -> Result<ContentRequestPageDto> {
        let mut f = ContentFilter {
            statuses: status_filter(q.status.as_deref())?,
            ..Default::default()
        };
        if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty() && *k != "all") {
            f.kind = Some(
                ContentRequestKind::parse(k)
                    .ok_or_else(|| Error::Validation(format!("unknown kind {k:?}")))?,
            );
        }
        f.user_id = if sees_all_requests(actor.permissions) {
            q.user_id
        } else {
            Some(actor.id)
        };
        let page = q.page.unwrap_or(1).max(1);
        let size = q.page_size.unwrap_or(25).clamp(1, 100);
        let asc = q.dir.as_deref() == Some("asc");
        let (rows, total) = self
            .content_repo()
            .page(
                &f,
                q.sort.as_deref().unwrap_or("created"),
                asc,
                size,
                u64::from(page - 1) * u64::from(size),
            )
            .await?;
        Ok(ContentRequestPageDto {
            items: self.content_request_dtos(rows).await?,
            total,
            page,
            page_size: size,
        })
    }

    /// Profiles and root folders for advanced book/album/radio requests. `kind`: ebook,
    /// audiobook, music or radio (audiobook profiles, radio root folders).
    pub async fn content_request_options(
        &self,
        actor: &Actor,
        kind: ContentKind,
    ) -> Result<RequestOptionsDto> {
        if !advanced_allowed(actor) {
            return Err(Error::Forbidden(
                "advanced requests are not allowed for your account".into(),
            ));
        }
        if !matches!(
            kind,
            ContentKind::Ebook | ContentKind::Audiobook | ContentKind::Music | ContentKind::Radio
        ) {
            return Err(Error::Validation(
                "kind must be ebook, audiobook, music or radio".into(),
            ));
        }
        let settings = self.request_settings().await?;
        let d = content_defaults(&settings, kind);
        let upstream = |e: anyhow::Error| Error::Upstream(format!("library: {e}"));
        let profiles: Vec<NamedIdDto> = self.library.content_profiles(kind).await.map_err(upstream)?;
        let root_folders = self.library.content_root_folders(kind).await.map_err(upstream)?;
        Ok(RequestOptionsDto {
            profiles,
            root_folders,
            default_profile_id: d.profile_id,
            default_root_folder_id: d.root_folder_id,
        })
    }

    // ------------------------------------------------------------ create

    /// Creates a book, album or radio show request. Auto-approves when allowed.
    pub async fn create_content_request(
        &self,
        actor: &Actor,
        dto: CreateContentRequestDto,
    ) -> Result<ContentRequestDto> {
        let perms = actor.permissions;
        let manager = perms.has(Permissions::MANAGE_REQUESTS);
        let kind = dto.kind;
        let target = match dto.user_id {
            Some(uid) if uid != actor.id => {
                if !manager {
                    return Err(Error::Forbidden(
                        "requesting for another user needs MANAGE_REQUESTS".into(),
                    ));
                }
                self.user_record(uid).await?
            }
            _ => self.user_record(actor.id).await?,
        };
        if !perms.has(content_permission(kind)) {
            return Err(Error::Forbidden(not_allowed(kind).into()));
        }
        let foreign_id = dto.foreign_id.trim().to_owned();
        if foreign_id.is_empty() {
            return Err(Error::Validation("foreign_id is required".into()));
        }
        let settings = self.request_settings().await?;
        let mut formats = match kind {
            ContentRequestKind::Book if dto.formats.is_empty() => default_formats(&settings),
            ContentRequestKind::Book => normalize_formats(&dto.formats),
            ContentRequestKind::Music | ContentRequestKind::Radio => Vec::new(),
        };
        let advanced = dto.profile_id.is_some() || dto.root_folder_id.is_some() || !dto.tags.is_empty();
        if advanced && !advanced_allowed(actor) {
            return Err(Error::Forbidden(
                "advanced request options are not allowed for your account".into(),
            ));
        }

        let (hit, _) = self
            .lookup_content(actor, kind, &foreign_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("{} not found", kind_word(kind))))?;
        let availability = self
            .library
            .content_availability(kind, std::slice::from_ref(&foreign_id))
            .await
            .map_err(|e| Error::Upstream(format!("library: {e}")))?
            .remove(&foreign_id)
            .unwrap_or_default();
        if availability.status == AvailabilityStatus::Available || hit.in_library.is_some() {
            return Err(Error::Conflict(match kind {
                ContentRequestKind::Radio => "this show is already followed".into(),
                _ => format!("this {} is already available", kind_word(kind)),
            }));
        }
        let existing = self
            .content_repo()
            .find(&ContentFilter {
                kind: Some(kind),
                foreign_ids: vec![foreign_id.clone()],
                ..Default::default()
            })
            .await?;
        let active: Vec<&ContentRow> = existing.iter().filter(|r| r.status.is_active()).collect();
        match kind {
            ContentRequestKind::Book => {
                let taken = requested_formats(active.iter().copied());
                formats.retain(|f| !taken.contains(f));
                if formats.is_empty() {
                    return Err(Error::Conflict(
                        "the requested formats have already been requested".into(),
                    ));
                }
                if formats.len() > 1 && (dto.profile_id.is_some() || dto.root_folder_id.is_some()) {
                    return Err(Error::Validation(
                        "a quality profile / root folder can only be chosen when requesting a single format"
                            .into(),
                    ));
                }
            }
            ContentRequestKind::Music | ContentRequestKind::Radio => {
                if !active.is_empty() {
                    return Err(Error::Conflict(format!(
                        "this {} has already been requested",
                        kind_word(kind)
                    )));
                }
            }
        }

        // Quota (managers are exempt; requesting on behalf counts against the target).
        if !manager {
            let q = self.quota_of(&target, &settings).await?;
            let usage = match kind {
                ContentRequestKind::Book => &q.book,
                ContentRequestKind::Music => &q.music,
                ContentRequestKind::Radio => &q.radio,
            };
            if let Some(limit) = usage.limit
                && usage.used + 1 > limit
            {
                return Err(Error::Forbidden(format!(
                    "{} request quota exceeded ({} of {limit} used in the last {} days)",
                    kind_word(kind),
                    usage.used,
                    usage.days
                )));
            }
        }

        let id = self
            .content_repo()
            .create(NewContent {
                user_id: target.id,
                kind,
                foreign_id,
                title: hit.title,
                subtitle: hit.subtitle,
                year: hit.year,
                poster: hit.cover,
                formats,
                profile_id: dto.profile_id,
                root_folder_id: dto.root_folder_id,
                tags: dto.tags,
                source: "manual",
            })
            .await?;
        let row = self.load_content(id).await?;
        self.changed("request", "created", id, Some(row.user_id));

        let auto = manager
            || match kind {
                ContentRequestKind::Book => settings.auto_approve_book,
                ContentRequestKind::Music => settings.auto_approve_music,
                ContentRequestKind::Radio => settings.auto_approve_radio,
            }
            || target.permissions.has(auto_approve_permission(kind));
        if auto {
            let row = self.approve_content_row(row, actor.id, &settings).await?;
            return self.content_request_dto(row).await;
        }
        self.notify_content(&row, NotifyKind::Pending).await;
        self.content_request_dto(row).await
    }

    // ------------------------------------------------------------ approve / decline / retry

    pub async fn approve_content_request(&self, actor: &Actor, id: i64) -> Result<ContentRequestDto> {
        require_manager(actor)?;
        let row = self.load_content(id).await?;
        if !matches!(row.status, RequestStatus::Pending | RequestStatus::Declined) {
            return Err(Error::Conflict(format!(
                "request is already {}",
                row.status.as_str()
            )));
        }
        let settings = self.request_settings().await?;
        let row = self.approve_content_row(row, actor.id, &settings).await?;
        self.content_request_dto(row).await
    }

    /// Re-runs the library add for a failed book/album/radio request.
    pub async fn retry_content_request(&self, actor: &Actor, id: i64) -> Result<ContentRequestDto> {
        require_manager(actor)?;
        let row = self.load_content(id).await?;
        if row.status != RequestStatus::Failed {
            return Err(Error::Conflict("only failed requests can be retried".into()));
        }
        let settings = self.request_settings().await?;
        let row = self.approve_content_row(row, actor.id, &settings).await?;
        self.content_request_dto(row).await
    }

    async fn approve_content_row(
        &self,
        mut row: ContentRow,
        by: i64,
        s: &RequestSettingsDto,
    ) -> Result<ContentRow> {
        row.modified_by = Some(by);
        row.decline_reason = None;
        row.approved_at = Some(now_ms());
        let mut tags = row.tags.clone();
        let mut add_tags = |d: &RequestDefaultsDto| {
            for t in &d.tags {
                if !tags.contains(t) {
                    tags.push(*t);
                }
            }
        };
        let added = match row.kind {
            ContentRequestKind::Book => {
                let formats = if row.formats.is_empty() {
                    BookFormat::ALL.to_vec()
                } else {
                    row.formats.clone()
                };
                for f in &formats {
                    add_tags(content_defaults(s, format_kind(*f)));
                }
                // Overrides apply to the single requested format.
                let single = (formats.len() == 1).then(|| formats[0]);
                let pick = |f: BookFormat| {
                    let d = content_defaults(s, format_kind(f));
                    if single == Some(f) {
                        (
                            row.profile_id.or(d.profile_id),
                            row.root_folder_id.or(d.root_folder_id),
                        )
                    } else {
                        (d.profile_id, d.root_folder_id)
                    }
                };
                let (ep, er) = pick(BookFormat::Ebook);
                let (ap, ar) = pick(BookFormat::Audiobook);
                self.library
                    .add_book(AddBookRequest {
                        work_id: row.foreign_id.clone(),
                        title: row.title.clone(),
                        formats: formats.clone(),
                        ebook_profile_id: ep,
                        ebook_root_folder_id: er,
                        audiobook_profile_id: ap,
                        audiobook_root_folder_id: ar,
                        tags,
                        request_id: row.id,
                    })
                    .await
            }
            ContentRequestKind::Music => {
                let d = &s.music;
                add_tags(d);
                self.library
                    .add_album(AddAlbumRequest {
                        release_group_id: row.foreign_id.clone(),
                        title: row.title.clone(),
                        quality_profile_id: row.profile_id.or(d.profile_id),
                        metadata_profile_id: s.music_metadata_profile_id,
                        root_folder_id: row.root_folder_id.or(d.root_folder_id),
                        tags,
                        request_id: row.id,
                    })
                    .await
            }
            ContentRequestKind::Radio => {
                let d = &s.radio;
                add_tags(d);
                match split_radio_foreign_id(&row.foreign_id) {
                    Some((provider, external_id)) => {
                        self.library
                            .add_radio(AddRadioRequest {
                                provider: provider.to_owned(),
                                external_id: external_id.to_owned(),
                                title: row.title.clone(),
                                quality_profile_id: row.profile_id.or(d.profile_id),
                                root_folder_id: row.root_folder_id.or(d.root_folder_id),
                                tags,
                                request_id: row.id,
                            })
                            .await
                    }
                    None => Err(anyhow::anyhow!("{} is not a radio show id", row.foreign_id)),
                }
            }
        };
        match added {
            Ok(a) => {
                row.status = RequestStatus::Approved;
                row.failure_reason = None;
                match row.kind {
                    ContentRequestKind::Book => {
                        row.book_id = Some(a.item_id);
                        row.author_id = a.parent_id.or(row.author_id);
                    }
                    ContentRequestKind::Music => {
                        row.album_id = Some(a.item_id);
                        row.artist_id = a.parent_id.or(row.artist_id);
                    }
                    ContentRequestKind::Radio => row.radio_show_id = Some(a.item_id),
                }
                if s.search_on_approve
                    && let Err(e) = self
                        .library
                        .content_search(row.kind, &[a.item_id], &row.formats)
                        .await
                {
                    tracing::warn!(request = row.id, error = %e, "could not start search for approved request");
                }
            }
            Err(e) => {
                tracing::warn!(request = row.id, error = %e, "adding approved request to the library failed");
                row.status = RequestStatus::Failed;
                row.failure_reason = Some(e.to_string());
            }
        }
        self.content_repo().save(&row).await?;
        let row = self.load_content(row.id).await?;
        self.changed("request", "updated", row.id, Some(row.user_id));
        if row.status == RequestStatus::Approved {
            self.notify_content(&row, NotifyKind::Approved).await;
            // The book/album may already be (partly) on disk; a followed show is available.
            if let Err(e) = self.sync_content_rows(vec![row.clone()]).await {
                tracing::debug!(error = %e, "post-approve status sync failed");
            }
            return self.load_content(row.id).await;
        }
        Ok(row)
    }

    pub async fn decline_content_request(
        &self,
        actor: &Actor,
        id: i64,
        reason: Option<String>,
    ) -> Result<ContentRequestDto> {
        require_manager(actor)?;
        let mut row = self.load_content(id).await?;
        if !matches!(row.status, RequestStatus::Pending | RequestStatus::Failed) {
            return Err(Error::Conflict(format!(
                "cannot decline a request that is {}",
                row.status.as_str()
            )));
        }
        row.status = RequestStatus::Declined;
        row.decline_reason = reason.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
        row.modified_by = Some(actor.id);
        self.content_repo().save(&row).await?;
        let row = self.load_content(id).await?;
        self.changed("request", "updated", id, Some(row.user_id));
        self.notify_content(&row, NotifyKind::Declined).await;
        self.content_request_dto(row).await
    }

    // ------------------------------------------------------------ edit / delete

    pub async fn update_content_request(
        &self,
        actor: &Actor,
        id: i64,
        dto: UpdateContentRequestDto,
    ) -> Result<ContentRequestDto> {
        let mut row = self.load_content(id).await?;
        let manager = actor.permissions.has(Permissions::MANAGE_REQUESTS);
        let owner = row.user_id == actor.id;
        if !manager && !(owner && row.status == RequestStatus::Pending) {
            if owner || sees_all_requests(actor.permissions) {
                return Err(Error::Forbidden("only pending requests can be edited".into()));
            }
            return Err(Error::NotFound("request not found".into()));
        }
        let advanced = dto.profile_id.is_some() || dto.root_folder_id.is_some() || dto.tags.is_some();
        if advanced && !advanced_allowed(actor) {
            return Err(Error::Forbidden(
                "advanced request options are not allowed for your account".into(),
            ));
        }
        if let Some(formats) = dto.formats {
            if row.kind != ContentRequestKind::Book {
                return Err(Error::Validation(format!(
                    "{} requests have no formats",
                    kind_word(row.kind)
                )));
            }
            let formats = normalize_formats(&formats);
            if formats.is_empty() {
                return Err(Error::Validation("at least one format is required".into()));
            }
            row.formats = formats;
        }
        if let Some(p) = dto.profile_id {
            row.profile_id = Some(p);
        }
        if let Some(r) = dto.root_folder_id {
            row.root_folder_id = Some(r);
        }
        if let Some(t) = dto.tags {
            row.tags = t;
        }
        if row.kind == ContentRequestKind::Book
            && row.formats.len() > 1
            && (row.profile_id.is_some() || row.root_folder_id.is_some())
        {
            return Err(Error::Validation(
                "a quality profile / root folder can only be chosen when requesting a single format".into(),
            ));
        }
        row.modified_by = Some(actor.id);
        self.content_repo().save(&row).await?;
        self.changed("request", "updated", id, Some(row.user_id));
        let row = self.load_content(id).await?;
        self.content_request_dto(row).await
    }

    pub async fn delete_content_request(&self, actor: &Actor, id: i64) -> Result<()> {
        let row = self.load_content(id).await?;
        if !actor.permissions.has(Permissions::MANAGE_REQUESTS) {
            if row.user_id != actor.id {
                return Err(Error::NotFound("request not found".into()));
            }
            if !matches!(
                row.status,
                RequestStatus::Pending | RequestStatus::Declined | RequestStatus::Failed
            ) {
                return Err(Error::Forbidden(
                    "approved requests can only be removed by a manager".into(),
                ));
            }
        }
        self.content_repo().delete(id).await?;
        self.changed("request", "deleted", id, Some(row.user_id));
        Ok(())
    }

    // ------------------------------------------------------------ status sync

    /// Updates approved / processing / partially available book, album and radio requests
    /// from library availability. Returns how many requests changed.
    pub async fn sync_content_statuses(&self) -> Result<usize> {
        let rows = self
            .content_repo()
            .find(&ContentFilter {
                statuses: vec![
                    RequestStatus::Approved,
                    RequestStatus::Processing,
                    RequestStatus::PartiallyAvailable,
                ],
                ..Default::default()
            })
            .await?;
        self.sync_content_rows(rows).await
    }

    async fn sync_content_rows(&self, rows: Vec<ContentRow>) -> Result<usize> {
        let mut changed = 0;
        for kind in ContentRequestKind::ALL {
            let of_kind: Vec<ContentRow> = rows.iter().filter(|r| r.kind == kind).cloned().collect();
            if of_kind.is_empty() {
                continue;
            }
            let mut ids: Vec<String> = of_kind.iter().map(|r| r.foreign_id.clone()).collect();
            ids.sort_unstable();
            ids.dedup();
            let avail: HashMap<String, Availability> = self
                .library
                .content_availability(kind, &ids)
                .await
                .map_err(|e| Error::Upstream(format!("library: {e}")))?;
            for mut row in of_kind {
                let Some(a) = avail.get(&row.foreign_id) else {
                    continue;
                };
                let Some(next) = movie_status(a) else { continue };
                if next == row.status {
                    continue;
                }
                row.status = next;
                if next == RequestStatus::Available {
                    row.available_at = Some(now_ms());
                }
                match kind {
                    ContentRequestKind::Book if row.book_id.is_none() => row.book_id = a.library_id,
                    ContentRequestKind::Music if row.album_id.is_none() => row.album_id = a.library_id,
                    ContentRequestKind::Radio if row.radio_show_id.is_none() => {
                        row.radio_show_id = a.library_id
                    }
                    _ => {}
                }
                self.content_repo().save(&row).await?;
                self.changed("request", "updated", row.id, Some(row.user_id));
                if next == RequestStatus::Available {
                    self.notify_content(&row, NotifyKind::Available).await;
                }
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ------------------------------------------------------------ notifications

    async fn notify_content(&self, row: &ContentRow, what: NotifyKind) {
        if let Err(e) = self.try_notify_content(row, what).await {
            tracing::warn!(request = row.id, error = %e, "request notification failed");
        }
    }

    async fn try_notify_content(&self, row: &ContentRow, what: NotifyKind) -> Result<()> {
        let users = self.users_repo();
        let requester = self.user_record(row.user_id).await?;
        let modified = match row.modified_by {
            Some(id) => users.get(id).await?.map(|u| u.display()),
            None => None,
        };
        let content = match row.kind {
            ContentRequestKind::Music => ContentKind::Music,
            ContentRequestKind::Radio => ContentKind::Radio,
            ContentRequestKind::Book if row.formats == [BookFormat::Audiobook] => ContentKind::Audiobook,
            ContentRequestKind::Book => ContentKind::Ebook,
        };
        let mut media = MediaSummary::content(content, row.title.clone());
        media.year = row.year;
        media.poster_url = row.poster.clone();
        media.id = row.library_id();
        media.overview = row.subtitle.as_ref().map(|s| match row.kind {
            ContentRequestKind::Radio => s.clone(),
            _ => format!("by {s}"),
        });
        let request = RequestSummary {
            id: row.id,
            status: notify_status(row.status),
            requested_by: requester.display(),
            requested_by_email: requester.email.clone(),
            seasons: Vec::new(),
            is_4k: false,
            modified_by: modified,
            comment: row.decline_reason.clone(),
        };
        let (event, pref, recipients) = match what {
            NotifyKind::Pending => (
                NotificationEvent::RequestPending { media, request },
                "request_pending",
                users.with_permission(Permissions::MANAGE_REQUESTS).await?,
            ),
            NotifyKind::Approved => (
                NotificationEvent::RequestApproved { media, request },
                "request_approved",
                vec![row.user_id],
            ),
            NotifyKind::Declined => (
                NotificationEvent::RequestDeclined { media, request },
                "request_declined",
                vec![row.user_id],
            ),
            NotifyKind::Available => (
                NotificationEvent::RequestAvailable { media, request },
                "request_available",
                vec![row.user_id],
            ),
        };
        let recipients = self.opted_in(recipients, pref).await?;
        self.notifications.notify(event, recipients).await;
        Ok(())
    }

    /// Book/album/radio requests for these foreign ids visible to `actor` (all with `all`),
    /// grouped.
    pub(crate) async fn content_requests_by_id(
        &self,
        actor: &Actor,
        kind: ContentRequestKind,
        ids: &[String],
        all: bool,
    ) -> Result<HashMap<String, Vec<ContentRow>>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .content_repo()
            .find(&ContentFilter {
                kind: Some(kind),
                foreign_ids: ids.to_vec(),
                user_id: (!all && !sees_all_requests(actor.permissions)).then_some(actor.id),
                ..Default::default()
            })
            .await?;
        let mut map: HashMap<String, Vec<ContentRow>> = HashMap::new();
        for r in rows {
            map.entry(r.foreign_id.clone()).or_default().push(r);
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requesters_only_have_public_links_read() {
        let user = |permissions| reelix_core::User {
            id: 1,
            username: "u".into(),
            email: None,
            display_name: None,
            auth_source: reelix_core::AuthSource::Local,
            permissions,
            created_at: chrono::Utc::now(),
        };
        let requester = user(Permissions::ROLE_USER | Permissions::REQUEST_RADIO);
        for bad in [
            "http://127.0.0.1/feed",
            "http://localhost/feed",
            "https://10.1.2.3/x",
            "https://192.168.0.5/x",
            "https://169.254.169.254/latest",
            "https://100.64.0.1/x",
            "https://[::1]/x",
            "https://[fd00::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://abs/feed",
            "https://nas.local/feed",
            "https://user@1.2.3.4/x",
            "ftp://1.2.3.4/x",
        ] {
            assert!(check_radio_link(&requester, bad).await.is_err(), "{bad}");
        }
        assert!(
            check_radio_link(&requester, "https://1.2.3.4/feed.xml")
                .await
                .is_ok()
        );
        assert!(check_radio_link(&requester, "just a minute").await.is_ok());
        assert!(check_radio_link(&requester, "b006s5dp").await.is_ok());
        let manager = user(Permissions::MANAGE_LIBRARY);
        assert!(check_radio_link(&manager, "http://127.0.0.1/feed").await.is_ok());
    }

    #[test]
    fn default_formats_fall_back_to_both() {
        let mut s = RequestSettingsDto::default();
        assert_eq!(default_formats(&s), BookFormat::ALL.to_vec());
        s.book_default_formats = vec![BookFormat::Audiobook, BookFormat::Audiobook];
        assert_eq!(default_formats(&s), vec![BookFormat::Audiobook]);
    }
}
