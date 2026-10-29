//! Request lifecycle: create (permissions, duplicates, availability, quotas, auto-approve),
//! approve / decline / edit / delete / retry, quotas and the status sync.

use std::collections::{BTreeSet, HashMap};

use reelix_api_types::requests::{
    AvailabilityStatus, CreateRequestDto, NamedIdDto, QuotaDto, QuotaUsageDto, RequestCountsDto,
    RequestDefaultsDto, RequestDto, RequestListQuery, RequestOptionsDto, RequestPageDto, RequestSettingsDto,
    RequestStatus, UpdateRequestDto, UserBriefDto,
};
use reelix_core::{MediaKind, Permissions};
use reelix_db::now_ms;
use reelix_notify::{MediaSummary, NotificationEvent, RequestSummary};

use crate::gateway::{AddMovieRequest, AddSeriesRequest, Availability};
use crate::repo::{
    NewRequest, RequestFilter, RequestRepo, RequestRow, UserAdminRepo, UserRecord, parse_kind, ts,
};
use crate::{Actor, Error, RequestService, Result};

const DAY_MS: i64 = 24 * 3600 * 1000;

/// Whether the actor sees every user's requests.
pub fn sees_all_requests(p: Permissions) -> bool {
    p.has(Permissions::VIEW_REQUESTS) || p.has(Permissions::MANAGE_REQUESTS)
}

fn kind_permission(kind: MediaKind) -> Permissions {
    match kind {
        MediaKind::Movie => Permissions::REQUEST_MOVIE,
        MediaKind::Series => Permissions::REQUEST_TV,
    }
}

fn auto_approve_permission(kind: MediaKind) -> Permissions {
    match kind {
        MediaKind::Movie => Permissions::AUTO_APPROVE_MOVIE,
        MediaKind::Series => Permissions::AUTO_APPROVE_TV,
    }
}

pub(crate) fn defaults_for(s: &RequestSettingsDto, kind: MediaKind, is_4k: bool) -> &RequestDefaultsDto {
    match (kind, is_4k) {
        (MediaKind::Movie, false) => &s.movie,
        (MediaKind::Movie, true) => &s.movie_4k,
        (MediaKind::Series, false) => &s.series,
        (MediaKind::Series, true) => &s.series_4k,
    }
}

pub(crate) fn four_k_enabled(s: &RequestSettingsDto, kind: MediaKind) -> bool {
    match kind {
        MediaKind::Movie => s.enable_4k_movie,
        MediaKind::Series => s.enable_4k_series,
    }
}

fn notify_status(s: RequestStatus) -> reelix_notify::event::RequestStatus {
    use reelix_notify::event::RequestStatus as N;
    match s {
        RequestStatus::Pending => N::Pending,
        RequestStatus::Declined => N::Declined,
        RequestStatus::Available => N::Available,
        _ => N::Approved,
    }
}

/// Status of a series request given per-season availability.
pub(crate) fn series_status(requested: &[u32], a: &Availability) -> Option<RequestStatus> {
    let per: Vec<AvailabilityStatus> = if a.seasons.is_empty() || requested.is_empty() {
        vec![a.status]
    } else {
        requested
            .iter()
            .map(|s| a.seasons.get(s).copied().unwrap_or_default())
            .collect()
    };
    if per.iter().all(|s| *s == AvailabilityStatus::Available) {
        Some(RequestStatus::Available)
    } else if per.iter().any(|s| {
        matches!(
            s,
            AvailabilityStatus::Available | AvailabilityStatus::PartiallyAvailable
        )
    }) {
        Some(RequestStatus::PartiallyAvailable)
    } else if per.contains(&AvailabilityStatus::Processing) || a.status == AvailabilityStatus::Processing {
        Some(RequestStatus::Processing)
    } else {
        None
    }
}

fn movie_status(a: &Availability) -> Option<RequestStatus> {
    match a.status {
        AvailabilityStatus::Available => Some(RequestStatus::Available),
        AvailabilityStatus::PartiallyAvailable => Some(RequestStatus::PartiallyAvailable),
        AvailabilityStatus::Processing => Some(RequestStatus::Processing),
        _ => None,
    }
}

impl RequestService {
    pub(crate) fn requests_repo(&self) -> RequestRepo<'_> {
        RequestRepo(&self.db)
    }

    pub(crate) fn users_repo(&self) -> UserAdminRepo<'_> {
        UserAdminRepo(&self.db)
    }

    pub(crate) async fn user_record(&self, id: i64) -> Result<UserRecord> {
        self.users_repo()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("user not found".into()))
    }

    /// Converts rows to DTOs (one user lookup for all rows).
    pub async fn request_dtos(&self, rows: Vec<RequestRow>) -> Result<Vec<RequestDto>> {
        let ids = rows
            .iter()
            .flat_map(|r| std::iter::once(r.user_id).chain(r.modified_by));
        let briefs = self.users_repo().briefs(ids).await?;
        Ok(rows.into_iter().map(|r| to_dto(r, &briefs)).collect())
    }

    async fn request_dto(&self, row: RequestRow) -> Result<RequestDto> {
        Ok(self.request_dtos(vec![row]).await?.remove(0))
    }

    async fn load_request(&self, id: i64) -> Result<RequestRow> {
        self.requests_repo()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("request not found".into()))
    }

    /// One request; requesters may only read their own.
    pub async fn get_request(&self, actor: &Actor, id: i64) -> Result<RequestDto> {
        let row = self.load_request(id).await?;
        if row.user_id != actor.id && !sees_all_requests(actor.permissions) {
            return Err(Error::NotFound("request not found".into()));
        }
        self.request_dto(row).await
    }

    pub async fn list_requests(&self, actor: &Actor, q: &RequestListQuery) -> Result<RequestPageDto> {
        let mut f = RequestFilter::default();
        match q.status.as_deref().unwrap_or("all") {
            "all" | "" => {}
            "active" => {
                f.statuses = vec![
                    RequestStatus::Pending,
                    RequestStatus::Approved,
                    RequestStatus::Processing,
                ]
            }
            other => {
                let s = RequestStatus::parse(other)
                    .ok_or_else(|| Error::Validation(format!("unknown status filter {other:?}")))?;
                f.statuses = vec![s];
            }
        }
        if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty() && *k != "all") {
            f.kind = Some(parse_kind(k).ok_or_else(|| Error::Validation(format!("unknown kind {k:?}")))?);
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
            .requests_repo()
            .page(
                &f,
                q.sort.as_deref().unwrap_or("created"),
                asc,
                size,
                u64::from(page - 1) * u64::from(size),
            )
            .await?;
        Ok(RequestPageDto {
            items: self.request_dtos(rows).await?,
            total,
            page,
            page_size: size,
        })
    }

    pub async fn request_counts(&self, actor: &Actor) -> Result<RequestCountsDto> {
        let scope = (!sees_all_requests(actor.permissions)).then_some(actor.id);
        let mut c = RequestCountsDto::default();
        for (status, kind, n) in self.requests_repo().counts(scope).await? {
            let n = n as u64;
            c.total += n;
            match status.as_str() {
                "pending" => c.pending += n,
                "approved" => c.approved += n,
                "declined" => c.declined += n,
                "processing" => c.processing += n,
                "partially_available" => c.partially_available += n,
                "available" => c.available += n,
                "failed" => c.failed += n,
                _ => {}
            }
            if kind == "movie" {
                c.movie += n;
            } else {
                c.series += n;
            }
        }
        Ok(c)
    }

    // ------------------------------------------------------------ quotas

    /// Quota usage of `user_id`. Requesters may only read their own.
    pub async fn quota(&self, actor: &Actor, user_id: i64) -> Result<QuotaDto> {
        if actor.id != user_id
            && !actor.permissions.has(Permissions::MANAGE_USERS)
            && !sees_all_requests(actor.permissions)
        {
            return Err(Error::Forbidden("you can only view your own quota".into()));
        }
        let user = self.user_record(user_id).await?;
        let settings = self.request_settings().await?;
        self.quota_of(&user, &settings).await
    }

    pub(crate) async fn quota_of(&self, user: &UserRecord, s: &RequestSettingsDto) -> Result<QuotaDto> {
        let usage = |limit: Option<u32>, days: u32, used: u32| {
            let remaining = limit.map(|l| l.saturating_sub(used));
            QuotaUsageDto {
                limit,
                days,
                used,
                remaining,
                restricted: remaining == Some(0),
            }
        };
        let movie_limit = user.movie_quota_limit.or(s.movie_quota_limit);
        let movie_days = user.movie_quota_days.unwrap_or(s.movie_quota_days).max(1);
        let tv_limit = user.tv_quota_limit.or(s.tv_quota_limit);
        let tv_days = user.tv_quota_days.unwrap_or(s.tv_quota_days).max(1);
        let now = now_ms();
        let movies = self
            .requests_repo()
            .since(user.id, MediaKind::Movie, now - i64::from(movie_days) * DAY_MS)
            .await?
            .len() as u32;
        let seasons: u32 = self
            .requests_repo()
            .since(user.id, MediaKind::Series, now - i64::from(tv_days) * DAY_MS)
            .await?
            .iter()
            .map(|r| r.seasons.len().max(1) as u32)
            .sum();
        Ok(QuotaDto {
            movie: usage(movie_limit, movie_days, movies),
            tv: usage(tv_limit, tv_days, seasons),
            exempt: user.permissions.has(Permissions::MANAGE_REQUESTS),
        })
    }

    // ------------------------------------------------------------ options

    pub async fn request_options(
        &self,
        actor: &Actor,
        kind: MediaKind,
        is_4k: bool,
    ) -> Result<RequestOptionsDto> {
        if !actor.permissions.has(Permissions::REQUEST_ADVANCED)
            && !actor.permissions.has(Permissions::MANAGE_REQUESTS)
        {
            return Err(Error::Forbidden(
                "advanced requests are not allowed for your account".into(),
            ));
        }
        let settings = self.request_settings().await?;
        let d = defaults_for(&settings, kind, is_4k);
        let profiles = self
            .db
            .quality_profiles()
            .list()
            .await?
            .into_iter()
            .map(|p| NamedIdDto {
                id: p.id,
                name: p.name,
            })
            .collect();
        let root_folders = self
            .library
            .root_folders(kind)
            .await
            .map_err(|e| Error::Upstream(format!("library: {e}")))?;
        Ok(RequestOptionsDto {
            profiles,
            root_folders,
            default_profile_id: d.profile_id,
            default_root_folder_id: d.root_folder_id,
        })
    }

    // ------------------------------------------------------------ create

    /// Creates a request (see module docs for the checks). Auto-approves when allowed.
    pub async fn create_request(&self, actor: &Actor, dto: CreateRequestDto) -> Result<RequestDto> {
        self.create_request_from(actor, dto, "manual").await
    }

    pub(crate) async fn create_request_from(
        &self,
        actor: &Actor,
        dto: CreateRequestDto,
        source: &'static str,
    ) -> Result<RequestDto> {
        let perms = actor.permissions;
        let manager = perms.has(Permissions::MANAGE_REQUESTS);
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
        let kind = dto.kind;
        if !perms.has(kind_permission(kind)) {
            return Err(Error::Forbidden(match kind {
                MediaKind::Movie => "you are not allowed to request movies".into(),
                MediaKind::Series => "you are not allowed to request series".into(),
            }));
        }
        let settings = self.request_settings().await?;
        if dto.is_4k {
            if !four_k_enabled(&settings, kind) {
                return Err(Error::Validation("4K requests are not enabled".into()));
            }
            if !perms.has(Permissions::REQUEST_4K) {
                return Err(Error::Forbidden("you are not allowed to request 4K".into()));
            }
        }
        let advanced = dto.profile_id.is_some() || dto.root_folder_id.is_some() || !dto.tags.is_empty();
        if advanced && !perms.has(Permissions::REQUEST_ADVANCED) && !manager {
            return Err(Error::Forbidden(
                "advanced request options are not allowed for your account".into(),
            ));
        }
        if let Some(pid) = dto.profile_id
            && self.db.quality_profiles().get(pid).await?.is_none()
        {
            return Err(Error::Validation(format!("unknown quality profile {pid}")));
        }

        // Metadata.
        let tmdb_id = dto.tmdb_id;
        let mut new = NewRequest {
            user_id: target.id,
            kind,
            tmdb_id,
            tvdb_id: None,
            imdb_id: None,
            title: String::new(),
            year: None,
            poster: None,
            backdrop: None,
            seasons: Vec::new(),
            is_4k: dto.is_4k,
            profile_id: dto.profile_id,
            root_folder_id: dto.root_folder_id,
            tags: dto.tags.clone(),
            source,
        };
        let availability = self
            .library
            .availability(kind, &[tmdb_id])
            .await
            .map_err(|e| Error::Upstream(format!("library: {e}")))?
            .remove(&tmdb_id)
            .unwrap_or_default();
        let existing = self
            .requests_repo()
            .find(&RequestFilter {
                kind: Some(kind),
                tmdb_ids: vec![tmdb_id],
                is_4k: Some(dto.is_4k),
                ..Default::default()
            })
            .await?;
        let active: Vec<&RequestRow> = existing.iter().filter(|r| r.status.is_active()).collect();
        match kind {
            MediaKind::Movie => {
                let m = self.metadata.movie(tmdb_id).await?;
                new.title = m.title;
                new.year = m.year;
                new.imdb_id = m.imdb_id;
                new.poster = m.images.poster;
                new.backdrop = m.images.backdrop;
                if !dto.is_4k && availability.status == AvailabilityStatus::Available {
                    return Err(Error::Conflict("this movie is already available".into()));
                }
                if !active.is_empty() {
                    return Err(Error::Conflict("this movie has already been requested".into()));
                }
            }
            MediaKind::Series => {
                let s = self.metadata.series(tmdb_id).await?;
                let regular: BTreeSet<u32> = s
                    .seasons
                    .iter()
                    .filter(|x| x.season_number > 0)
                    .map(|x| x.season_number)
                    .collect();
                if regular.is_empty() {
                    return Err(Error::Validation("this series has no seasons to request".into()));
                }
                let mut wanted: BTreeSet<u32> = match (&dto.seasons, settings.partial_requests) {
                    (Some(list), true) if !list.is_empty() => list.iter().copied().collect(),
                    _ => regular.clone(),
                };
                if let Some(bad) = wanted.iter().find(|n| !regular.contains(n)) {
                    return Err(Error::Validation(format!("season {bad} does not exist")));
                }
                if !dto.is_4k {
                    wanted.retain(|n| availability.seasons.get(n) != Some(&AvailabilityStatus::Available));
                    if availability.status == AvailabilityStatus::Available && availability.seasons.is_empty()
                    {
                        wanted.clear();
                    }
                    if wanted.is_empty() {
                        return Err(Error::Conflict(
                            "the requested seasons are already available".into(),
                        ));
                    }
                }
                for r in &active {
                    for n in &r.seasons {
                        wanted.remove(n);
                    }
                }
                if wanted.is_empty() {
                    return Err(Error::Conflict(
                        "the requested seasons have already been requested".into(),
                    ));
                }
                new.seasons = wanted.into_iter().collect();
                new.title = s.title;
                new.year = s.year;
                new.imdb_id = s.imdb_id;
                new.tvdb_id = s.tvdb_id;
                new.poster = s.images.poster;
                new.backdrop = s.images.backdrop;
            }
        }

        // Quota (managers are exempt; requesting on behalf counts against the target).
        if !manager {
            let q = self.quota_of(&target, &settings).await?;
            let (usage, cost, what) = match kind {
                MediaKind::Movie => (&q.movie, 1, "movie"),
                MediaKind::Series => (&q.tv, new.seasons.len().max(1) as u32, "season"),
            };
            if let Some(limit) = usage.limit
                && usage.used + cost > limit
            {
                return Err(Error::Forbidden(format!(
                    "{what} request quota exceeded ({} of {limit} used in the last {} days)",
                    usage.used, usage.days
                )));
            }
        }

        let id = self.requests_repo().create(new).await?;
        let row = self.load_request(id).await?;
        self.changed("request", "created", id, Some(row.user_id));

        let auto = manager
            || match kind {
                MediaKind::Movie => settings.auto_approve_movie,
                MediaKind::Series => settings.auto_approve_series,
            }
            || (target.permissions.has(auto_approve_permission(kind))
                && (!row.is_4k || target.permissions.has(Permissions::AUTO_APPROVE_4K)));
        if auto {
            let row = self.approve_row(row, actor.id, &settings).await?;
            return self.request_dto(row).await;
        }
        self.notify_request(&row, NotifyKind::Pending).await;
        self.request_dto(row).await
    }

    // ------------------------------------------------------------ approve / decline

    pub async fn approve_request(&self, actor: &Actor, id: i64) -> Result<RequestDto> {
        require_manager(actor)?;
        let row = self.load_request(id).await?;
        if !matches!(row.status, RequestStatus::Pending | RequestStatus::Declined) {
            return Err(Error::Conflict(format!(
                "request is already {}",
                row.status.as_str()
            )));
        }
        let settings = self.request_settings().await?;
        let row = self.approve_row(row, actor.id, &settings).await?;
        self.request_dto(row).await
    }

    /// Re-runs the library add for a failed request.
    pub async fn retry_request(&self, actor: &Actor, id: i64) -> Result<RequestDto> {
        require_manager(actor)?;
        let row = self.load_request(id).await?;
        if row.status != RequestStatus::Failed {
            return Err(Error::Conflict("only failed requests can be retried".into()));
        }
        let settings = self.request_settings().await?;
        let row = self.approve_row(row, actor.id, &settings).await?;
        self.request_dto(row).await
    }

    async fn approve_row(&self, mut row: RequestRow, by: i64, s: &RequestSettingsDto) -> Result<RequestRow> {
        let d = defaults_for(s, row.kind, row.is_4k);
        let profile = row.profile_id.or(d.profile_id);
        let root = row.root_folder_id.or(d.root_folder_id);
        let mut tags = row.tags.clone();
        for t in &d.tags {
            if !tags.contains(t) {
                tags.push(*t);
            }
        }
        row.modified_by = Some(by);
        row.decline_reason = None;
        row.approved_at = Some(now_ms());
        let added = match row.kind {
            MediaKind::Movie => self
                .library
                .add_movie(AddMovieRequest {
                    tmdb_id: row.tmdb_id,
                    title: row.title.clone(),
                    year: row.year,
                    quality_profile_id: profile,
                    root_folder_id: root,
                    tags,
                    minimum_availability: s.minimum_availability.clone(),
                    is_4k: row.is_4k,
                    request_id: row.id,
                })
                .await
                .map(|id| (MediaKind::Movie, id)),
            MediaKind::Series => self
                .library
                .add_series(AddSeriesRequest {
                    tmdb_id: row.tmdb_id,
                    tvdb_id: row.tvdb_id,
                    title: row.title.clone(),
                    year: row.year,
                    quality_profile_id: profile,
                    root_folder_id: root,
                    tags,
                    seasons: row.seasons.clone(),
                    season_folder: s.season_folder,
                    series_type: s.series_type.clone(),
                    is_4k: row.is_4k,
                    request_id: row.id,
                })
                .await
                .map(|id| (MediaKind::Series, id)),
        };
        match added {
            Ok((kind, lib_id)) => {
                row.status = RequestStatus::Approved;
                row.failure_reason = None;
                match kind {
                    MediaKind::Movie => row.movie_id = Some(lib_id),
                    MediaKind::Series => row.series_id = Some(lib_id),
                }
                if s.search_on_approve
                    && let Err(e) = self.library.request_search(kind, &[lib_id]).await
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
        self.requests_repo().save(&row).await?;
        let row = self.load_request(row.id).await?;
        self.changed("request", "updated", row.id, Some(row.user_id));
        if row.status == RequestStatus::Approved {
            self.notify_request(&row, NotifyKind::Approved).await;
            // The title may already be (partly) on disk.
            if let Err(e) = self.sync_rows(vec![row.clone()]).await {
                tracing::debug!(error = %e, "post-approve status sync failed");
            }
            return self.load_request(row.id).await;
        }
        Ok(row)
    }

    pub async fn decline_request(
        &self,
        actor: &Actor,
        id: i64,
        reason: Option<String>,
    ) -> Result<RequestDto> {
        require_manager(actor)?;
        let mut row = self.load_request(id).await?;
        if !matches!(row.status, RequestStatus::Pending | RequestStatus::Failed) {
            return Err(Error::Conflict(format!(
                "cannot decline a request that is {}",
                row.status.as_str()
            )));
        }
        row.status = RequestStatus::Declined;
        row.decline_reason = reason.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
        row.modified_by = Some(actor.id);
        self.requests_repo().save(&row).await?;
        let row = self.load_request(id).await?;
        self.changed("request", "updated", id, Some(row.user_id));
        self.notify_request(&row, NotifyKind::Declined).await;
        self.request_dto(row).await
    }

    // ------------------------------------------------------------ edit / delete

    pub async fn update_request(&self, actor: &Actor, id: i64, dto: UpdateRequestDto) -> Result<RequestDto> {
        let mut row = self.load_request(id).await?;
        let manager = actor.permissions.has(Permissions::MANAGE_REQUESTS);
        let owner = row.user_id == actor.id;
        if !manager && !(owner && row.status == RequestStatus::Pending) {
            if owner || sees_all_requests(actor.permissions) {
                return Err(Error::Forbidden("only pending requests can be edited".into()));
            }
            return Err(Error::NotFound("request not found".into()));
        }
        let advanced = dto.profile_id.is_some() || dto.root_folder_id.is_some() || dto.tags.is_some();
        if advanced && !manager && !actor.permissions.has(Permissions::REQUEST_ADVANCED) {
            return Err(Error::Forbidden(
                "advanced request options are not allowed for your account".into(),
            ));
        }
        if let Some(seasons) = dto.seasons {
            if row.kind != MediaKind::Series {
                return Err(Error::Validation("movies have no seasons".into()));
            }
            let set: BTreeSet<u32> = seasons.into_iter().filter(|s| *s > 0).collect();
            if set.is_empty() {
                return Err(Error::Validation("at least one season is required".into()));
            }
            row.seasons = set.into_iter().collect();
        }
        if let Some(b) = dto.is_4k
            && b != row.is_4k
        {
            let settings = self.request_settings().await?;
            if b && (!four_k_enabled(&settings, row.kind) || !actor.permissions.has(Permissions::REQUEST_4K))
            {
                return Err(Error::Forbidden("4K requests are not allowed".into()));
            }
            row.is_4k = b;
        }
        if let Some(p) = dto.profile_id {
            if self.db.quality_profiles().get(p).await?.is_none() {
                return Err(Error::Validation(format!("unknown quality profile {p}")));
            }
            row.profile_id = Some(p);
        }
        if let Some(r) = dto.root_folder_id {
            row.root_folder_id = Some(r);
        }
        if let Some(t) = dto.tags {
            row.tags = t;
        }
        row.modified_by = Some(actor.id);
        self.requests_repo().save(&row).await?;
        self.changed("request", "updated", id, Some(row.user_id));
        let row = self.load_request(id).await?;
        self.request_dto(row).await
    }

    pub async fn delete_request(&self, actor: &Actor, id: i64) -> Result<()> {
        let row = self.load_request(id).await?;
        let manager = actor.permissions.has(Permissions::MANAGE_REQUESTS);
        if !manager {
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
        self.requests_repo().delete(id).await?;
        self.changed("request", "deleted", id, Some(row.user_id));
        Ok(())
    }

    // ------------------------------------------------------------ status sync

    /// Updates approved / processing / partially available requests from library
    /// availability. Returns how many requests changed.
    pub async fn sync_request_statuses(&self) -> Result<usize> {
        let rows = self
            .requests_repo()
            .find(&RequestFilter {
                statuses: vec![
                    RequestStatus::Approved,
                    RequestStatus::Processing,
                    RequestStatus::PartiallyAvailable,
                ],
                ..Default::default()
            })
            .await?;
        self.sync_rows(rows).await
    }

    async fn sync_rows(&self, rows: Vec<RequestRow>) -> Result<usize> {
        let mut changed = 0;
        for kind in [MediaKind::Movie, MediaKind::Series] {
            let of_kind: Vec<RequestRow> = rows.iter().filter(|r| r.kind == kind).cloned().collect();
            if of_kind.is_empty() {
                continue;
            }
            let mut ids: Vec<u32> = of_kind.iter().map(|r| r.tmdb_id).collect();
            ids.sort_unstable();
            ids.dedup();
            let avail = self
                .library
                .availability(kind, &ids)
                .await
                .map_err(|e| Error::Upstream(format!("library: {e}")))?;
            for mut row in of_kind {
                let Some(a) = avail.get(&row.tmdb_id) else {
                    continue;
                };
                let next = match kind {
                    MediaKind::Movie => movie_status(a),
                    MediaKind::Series => series_status(&row.seasons, a),
                };
                let Some(next) = next else { continue };
                if next == row.status {
                    continue;
                }
                row.status = next;
                if next == RequestStatus::Available {
                    row.available_at = Some(now_ms());
                }
                if row.movie_id.is_none() && kind == MediaKind::Movie {
                    row.movie_id = a.library_id;
                }
                if row.series_id.is_none() && kind == MediaKind::Series {
                    row.series_id = a.library_id;
                }
                self.requests_repo().save(&row).await?;
                self.changed("request", "updated", row.id, Some(row.user_id));
                if next == RequestStatus::Available {
                    self.notify_request(&row, NotifyKind::Available).await;
                }
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ------------------------------------------------------------ notifications

    pub(crate) async fn notify_request(&self, row: &RequestRow, what: NotifyKind) {
        if let Err(e) = self.try_notify_request(row, what).await {
            tracing::warn!(request = row.id, error = %e, "request notification failed");
        }
    }

    async fn try_notify_request(&self, row: &RequestRow, what: NotifyKind) -> Result<()> {
        let users = self.users_repo();
        let requester = self.user_record(row.user_id).await?;
        let modified = match row.modified_by {
            Some(id) => users.get(id).await?.map(|u| u.display()),
            None => None,
        };
        let mut media = MediaSummary::new(row.kind, row.title.clone());
        media.year = row.year;
        media.tmdb_id = Some(i64::from(row.tmdb_id));
        media.tvdb_id = row.tvdb_id.map(i64::from);
        media.imdb_id = row.imdb_id.clone();
        media.poster_url = row.poster.clone();
        media.id = row.movie_id.or(row.series_id);
        let request = RequestSummary {
            id: row.id,
            status: notify_status(row.status),
            requested_by: requester.display(),
            requested_by_email: requester.email.clone(),
            seasons: row.seasons.clone(),
            is_4k: row.is_4k,
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

    /// Filters `users` to those whose preferences enable `event`.
    pub(crate) async fn opted_in(&self, users: Vec<i64>, event: &str) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        for u in users {
            if self
                .users_repo()
                .prefs(u)
                .await?
                .events
                .iter()
                .any(|e| e == event)
            {
                out.push(u);
            }
        }
        Ok(out)
    }

    /// Requests for these TMDB ids visible to `actor`, grouped by id.
    pub(crate) async fn requests_by_tmdb(
        &self,
        actor: &Actor,
        kind: MediaKind,
        tmdb_ids: &[u32],
    ) -> Result<HashMap<u32, Vec<RequestRow>>> {
        if tmdb_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .requests_repo()
            .find(&RequestFilter {
                kind: Some(kind),
                tmdb_ids: tmdb_ids.to_vec(),
                user_id: (!sees_all_requests(actor.permissions)).then_some(actor.id),
                ..Default::default()
            })
            .await?;
        let mut map: HashMap<u32, Vec<RequestRow>> = HashMap::new();
        for r in rows {
            map.entry(r.tmdb_id).or_default().push(r);
        }
        Ok(map)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifyKind {
    Pending,
    Approved,
    Declined,
    Available,
}

fn require_manager(actor: &Actor) -> Result<()> {
    if actor.permissions.has(Permissions::MANAGE_REQUESTS) {
        Ok(())
    } else {
        Err(Error::Forbidden("managing requests needs MANAGE_REQUESTS".into()))
    }
}

fn unknown_user(id: i64) -> UserBriefDto {
    UserBriefDto {
        id,
        username: "deleted".into(),
        display_name: "Deleted user".into(),
        avatar: None,
    }
}

pub(crate) fn to_dto(r: RequestRow, briefs: &HashMap<i64, UserBriefDto>) -> RequestDto {
    RequestDto {
        id: r.id,
        kind: r.kind,
        tmdb_id: r.tmdb_id,
        tvdb_id: r.tvdb_id,
        imdb_id: r.imdb_id,
        title: r.title,
        year: r.year,
        poster: r.poster,
        backdrop: r.backdrop,
        seasons: r.seasons,
        status: r.status,
        is_4k: r.is_4k,
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
        movie_id: r.movie_id,
        series_id: r.series_id,
        created_at: ts(r.created_at),
        updated_at: ts(r.updated_at),
        approved_at: r.approved_at.map(ts),
        available_at: r.available_at.map(ts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_status_rules() {
        let mut a = Availability {
            status: AvailabilityStatus::PartiallyAvailable,
            library_id: Some(1),
            seasons: [
                (1, AvailabilityStatus::Available),
                (2, AvailabilityStatus::Monitored),
            ]
            .into(),
        };
        assert_eq!(series_status(&[1], &a), Some(RequestStatus::Available));
        assert_eq!(
            series_status(&[1, 2], &a),
            Some(RequestStatus::PartiallyAvailable)
        );
        assert_eq!(series_status(&[2], &a), None);
        a.seasons.insert(2, AvailabilityStatus::Processing);
        assert_eq!(series_status(&[2], &a), Some(RequestStatus::Processing));
        let whole = Availability {
            status: AvailabilityStatus::Available,
            ..Default::default()
        };
        assert_eq!(series_status(&[1, 2], &whole), Some(RequestStatus::Available));
    }
}
