//! Issues (problems with available media) and their comment threads.

use reelix_api_types::requests::{
    CreateCommentDto, CreateIssueDto, IssueCommentDto, IssueCountsDto, IssueDto, IssueListQuery,
    IssuePageDto, IssueStatus, IssueType, UpdateIssueDto, UserBriefDto,
};
use reelix_core::{MediaKind, Permissions};
use reelix_db::now_ms;
use reelix_notify::{IssueSummary, MediaSummary, NotificationEvent};

use crate::repo::{IssueFilter, IssueRepo, IssueRow, parse_kind, ts};
use crate::{Actor, Error, RequestService, Result};

const MAX_MESSAGE: usize = 4000;

fn manages_issues(p: Permissions) -> bool {
    p.has(Permissions::MANAGE_ISSUES)
}

fn clean_message(m: &str) -> Result<String> {
    let m = m.trim();
    if m.is_empty() {
        return Err(Error::Validation("message must not be empty".into()));
    }
    if m.chars().count() > MAX_MESSAGE {
        return Err(Error::Validation(format!(
            "message is longer than {MAX_MESSAGE} characters"
        )));
    }
    Ok(m.to_owned())
}

fn notify_type(t: IssueType) -> reelix_notify::event::IssueType {
    use reelix_notify::event::IssueType as N;
    match t {
        IssueType::Video => N::Video,
        IssueType::Audio => N::Audio,
        IssueType::Subtitles => N::Subtitles,
        IssueType::Other => N::Other,
    }
}

impl RequestService {
    fn issues_repo(&self) -> IssueRepo<'_> {
        IssueRepo(&self.db)
    }

    async fn load_issue(&self, actor: &Actor, id: i64) -> Result<IssueRow> {
        let i = self
            .issues_repo()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("issue not found".into()))?;
        if i.user_id != actor.id && !manages_issues(actor.permissions) {
            return Err(Error::NotFound("issue not found".into()));
        }
        Ok(i)
    }

    async fn issue_dtos(&self, rows: Vec<IssueRow>, with_comments: bool) -> Result<Vec<IssueDto>> {
        let mut out = Vec::with_capacity(rows.len());
        let mut comments = Vec::new();
        if with_comments {
            for r in &rows {
                comments.push(self.issues_repo().comments(r.id).await?);
            }
        }
        let ids = rows
            .iter()
            .flat_map(|r| std::iter::once(r.user_id).chain(r.resolved_by))
            .chain(comments.iter().flatten().map(|c| c.user_id));
        let briefs = self.users_repo().briefs(ids.collect::<Vec<_>>()).await?;
        let user = |id: i64| {
            briefs.get(&id).cloned().unwrap_or(UserBriefDto {
                id,
                username: "deleted".into(),
                display_name: "Deleted user".into(),
                avatar: None,
            })
        };
        let mut comments = comments.into_iter();
        for r in rows {
            let thread = comments.next().unwrap_or_default();
            out.push(IssueDto {
                id: r.id,
                kind: r.kind,
                tmdb_id: r.tmdb_id,
                title: r.title,
                year: r.year,
                poster: r.poster,
                issue_type: r.issue_type,
                status: r.status,
                season: r.season,
                episode: r.episode,
                message: r.message,
                reported_by: user(r.user_id),
                resolved_by: r.resolved_by.map(user),
                comment_count: r.comment_count,
                comments: thread
                    .into_iter()
                    .map(|c| IssueCommentDto {
                        id: c.id,
                        user: user(c.user_id),
                        message: c.message,
                        created_at: ts(c.created_at),
                    })
                    .collect(),
                created_at: ts(r.created_at),
                updated_at: ts(r.updated_at),
                resolved_at: r.resolved_at.map(ts),
            });
        }
        Ok(out)
    }

    async fn issue_detail(&self, id: i64) -> Result<IssueDto> {
        let row = self
            .issues_repo()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("issue not found".into()))?;
        Ok(self.issue_dtos(vec![row], true).await?.remove(0))
    }

    pub async fn list_issues(&self, actor: &Actor, q: &IssueListQuery) -> Result<IssuePageDto> {
        let mut f = IssueFilter::default();
        match q.status.as_deref().unwrap_or("all") {
            "all" | "" => {}
            s => {
                f.status = Some(
                    IssueStatus::parse(s)
                        .ok_or_else(|| Error::Validation(format!("unknown status {s:?}")))?,
                )
            }
        }
        if let Some(t) = q.issue_type.as_deref().filter(|t| !t.is_empty() && *t != "all") {
            f.issue_type = Some(
                IssueType::parse(t).ok_or_else(|| Error::Validation(format!("unknown issue type {t:?}")))?,
            );
        }
        if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty() && *k != "all") {
            f.kind = Some(parse_kind(k).ok_or_else(|| Error::Validation(format!("unknown kind {k:?}")))?);
        }
        f.tmdb_id = q.tmdb_id;
        if !manages_issues(actor.permissions) {
            f.user_id = Some(actor.id);
        }
        let page = q.page.unwrap_or(1).max(1);
        let size = q.page_size.unwrap_or(25).clamp(1, 100);
        let (rows, total) = self
            .issues_repo()
            .page(&f, size, u64::from(page - 1) * u64::from(size))
            .await?;
        Ok(IssuePageDto {
            items: self.issue_dtos(rows, false).await?,
            total,
            page,
            page_size: size,
        })
    }

    pub async fn issue_counts(&self, actor: &Actor) -> Result<IssueCountsDto> {
        let scope = (!manages_issues(actor.permissions)).then_some(actor.id);
        let mut c = IssueCountsDto::default();
        for (status, t, n) in self.issues_repo().counts(scope).await? {
            let n = n as u64;
            c.total += n;
            if status == "open" {
                c.open += n;
            } else {
                c.resolved += n;
            }
            match t.as_str() {
                "video" => c.video += n,
                "audio" => c.audio += n,
                "subtitles" => c.subtitles += n,
                _ => c.other += n,
            }
        }
        Ok(c)
    }

    pub async fn get_issue(&self, actor: &Actor, id: i64) -> Result<IssueDto> {
        self.load_issue(actor, id).await?;
        self.issue_detail(id).await
    }

    pub async fn create_issue(&self, actor: &Actor, dto: CreateIssueDto) -> Result<IssueDto> {
        if !actor.permissions.has(Permissions::CREATE_ISSUES) && !manages_issues(actor.permissions) {
            return Err(Error::Forbidden("you are not allowed to report issues".into()));
        }
        let message = clean_message(&dto.message)?;
        if dto.kind == MediaKind::Movie && (dto.season.is_some() || dto.episode.is_some()) {
            return Err(Error::Validation("movies have no seasons or episodes".into()));
        }
        if dto.episode.is_some() && dto.season.is_none() {
            return Err(Error::Validation("an episode needs a season".into()));
        }
        let (title, year, poster) = match dto.kind {
            MediaKind::Movie => {
                let m = self.metadata.movie(dto.tmdb_id).await?;
                (m.title, m.year, m.images.poster)
            }
            MediaKind::Series => {
                let s = self.metadata.series(dto.tmdb_id).await?;
                (s.title, s.year, s.images.poster)
            }
        };
        let id = self
            .issues_repo()
            .create(crate::repo::NewIssue {
                user_id: actor.id,
                kind: dto.kind,
                tmdb_id: dto.tmdb_id,
                title,
                year,
                poster,
                issue_type: dto.issue_type,
                season: dto.season,
                episode: dto.episode,
                message,
            })
            .await?;
        self.changed("issue", "created", id, Some(actor.id));
        let dto = self.issue_detail(id).await?;
        self.notify_issue(&dto, false).await;
        Ok(dto)
    }

    pub async fn update_issue(&self, actor: &Actor, id: i64, dto: UpdateIssueDto) -> Result<IssueDto> {
        let mut row = self.load_issue(actor, id).await?;
        if let Some(t) = dto.issue_type {
            row.issue_type = t;
        }
        if let Some(m) = dto.message {
            row.message = clean_message(&m)?;
        }
        if row.kind == MediaKind::Series {
            if dto.season.is_some() {
                row.season = dto.season;
            }
            if dto.episode.is_some() {
                row.episode = dto.episode;
            }
        }
        self.issues_repo().save(&row).await?;
        self.changed("issue", "updated", id, Some(row.user_id));
        self.issue_detail(id).await
    }

    pub async fn delete_issue(&self, actor: &Actor, id: i64) -> Result<()> {
        let row = self.load_issue(actor, id).await?;
        if !manages_issues(actor.permissions) && row.comment_count > 0 {
            return Err(Error::Forbidden(
                "issues with comments can only be removed by a manager".into(),
            ));
        }
        self.issues_repo().delete(id).await?;
        self.changed("issue", "deleted", id, Some(row.user_id));
        Ok(())
    }

    pub async fn add_comment(&self, actor: &Actor, id: i64, dto: CreateCommentDto) -> Result<IssueDto> {
        let row = self.load_issue(actor, id).await?;
        let message = clean_message(&dto.message)?;
        self.issues_repo().add_comment(id, actor.id, &message).await?;
        self.issues_repo().touch(id).await?;
        self.changed("issue", "updated", id, Some(row.user_id));
        if row.user_id != actor.id {
            let to = self.opted_in(vec![row.user_id], "issue_comment").await?;
            if !to.is_empty() {
                let dto = self.issue_detail(id).await?;
                let (media, mut issue) = issue_summary(&dto);
                issue.comment = Some(message);
                // No dedicated "comment" event exists; managers' replies reuse IssueReported.
                self.notifications
                    .notify(NotificationEvent::IssueReported { media, issue }, to)
                    .await;
            }
        }
        self.issue_detail(id).await
    }

    pub async fn set_issue_status(&self, actor: &Actor, id: i64, status: IssueStatus) -> Result<IssueDto> {
        let mut row = self.load_issue(actor, id).await?;
        if row.status == status {
            return self.issue_detail(id).await;
        }
        row.status = status;
        match status {
            IssueStatus::Resolved => {
                row.resolved_by = Some(actor.id);
                row.resolved_at = Some(now_ms());
            }
            IssueStatus::Open => {
                row.resolved_by = None;
                row.resolved_at = None;
            }
        }
        self.issues_repo().save(&row).await?;
        self.changed("issue", "updated", id, Some(row.user_id));
        let dto = self.issue_detail(id).await?;
        if status == IssueStatus::Resolved {
            self.notify_issue(&dto, true).await;
        }
        Ok(dto)
    }

    async fn notify_issue(&self, dto: &IssueDto, resolved: bool) {
        let res: Result<()> = async {
            let (media, issue) = issue_summary(dto);
            if resolved {
                let to = self.opted_in(vec![dto.reported_by.id], "issue_resolved").await?;
                self.notifications
                    .notify(NotificationEvent::IssueResolved { media, issue }, to)
                    .await;
            } else {
                let managers = self
                    .users_repo()
                    .with_permission(Permissions::MANAGE_ISSUES)
                    .await?;
                let to = self.opted_in(managers, "issue_reported").await?;
                self.notifications
                    .notify(NotificationEvent::IssueReported { media, issue }, to)
                    .await;
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            tracing::warn!(issue = dto.id, error = %e, "issue notification failed");
        }
    }
}

fn issue_summary(d: &IssueDto) -> (MediaSummary, IssueSummary) {
    let mut media = MediaSummary::new(d.kind, d.title.clone());
    media.year = d.year;
    media.tmdb_id = Some(i64::from(d.tmdb_id));
    media.poster_url = d.poster.clone();
    let issue = IssueSummary {
        id: d.id,
        issue_type: notify_type(d.issue_type),
        message: d.message.clone(),
        reported_by: d.reported_by.display_name.clone(),
        resolved_by: d.resolved_by.as_ref().map(|u| u.display_name.clone()),
        comment: None,
    };
    (media, issue)
}
