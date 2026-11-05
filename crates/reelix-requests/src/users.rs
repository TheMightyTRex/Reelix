//! User management: list/create/update/delete, role presets, media-server import and
//! per-user notification preferences.

use reelix_api_types::requests::{
    CreateUserDto, ExternalUserDto, ImportServerDto, ImportUsersDto, ImportUsersResultDto,
    NotificationPrefsDto, PermissionInfoDto, RolePresetDto, RolesDto, USER_NOTIFICATION_EVENTS,
    UpdateUserDto, UserDetailDto,
};
use reelix_core::{AuthSource, Permissions};
use reelix_db::NewUser;
use reelix_mediaserver::MediaServerKind;

use crate::repo::{UserEdit, UserRecord, ts};
use crate::{Actor, Error, MediaServerEntry, RequestService, Result};

/// A new password for [`RequestService::update_user`].
#[derive(Debug, Clone)]
pub struct PasswordChange {
    /// Argon2 hash, computed by the caller.
    pub hash: String,
    /// Token hash of the session to keep when users change their own password (the caller's).
    pub keep_session: Option<String>,
}

/// `(constant, label, description)` for every permission bit.
pub const PERMISSION_INFO: &[(Permissions, &str, &str, &str)] = &[
    (
        Permissions::ADMIN,
        "ADMIN",
        "Admin",
        "Full access to everything, including system settings",
    ),
    (
        Permissions::MANAGE_LIBRARY,
        "MANAGE_LIBRARY",
        "Manage library",
        "Movies, series, indexers, downloads",
    ),
    (
        Permissions::MANAGE_REQUESTS,
        "MANAGE_REQUESTS",
        "Manage requests",
        "Approve, decline and edit requests",
    ),
    (
        Permissions::MANAGE_USERS,
        "MANAGE_USERS",
        "Manage users",
        "Create, edit and delete users",
    ),
    (
        Permissions::MANAGE_ISSUES,
        "MANAGE_ISSUES",
        "Manage issues",
        "See, comment on and resolve all issues",
    ),
    (
        Permissions::REQUEST_MOVIE,
        "REQUEST_MOVIE",
        "Request movies",
        "Submit movie requests",
    ),
    (
        Permissions::REQUEST_TV,
        "REQUEST_TV",
        "Request series",
        "Submit series requests",
    ),
    (
        Permissions::REQUEST_4K,
        "REQUEST_4K",
        "Request 4K",
        "Submit 4K requests",
    ),
    (
        Permissions::REQUEST_ADVANCED,
        "REQUEST_ADVANCED",
        "Advanced requests",
        "Choose quality profile and root folder",
    ),
    (
        Permissions::AUTO_APPROVE_MOVIE,
        "AUTO_APPROVE_MOVIE",
        "Auto-approve movies",
        "Movie requests are approved automatically",
    ),
    (
        Permissions::AUTO_APPROVE_TV,
        "AUTO_APPROVE_TV",
        "Auto-approve series",
        "Series requests are approved automatically",
    ),
    (
        Permissions::AUTO_APPROVE_4K,
        "AUTO_APPROVE_4K",
        "Auto-approve 4K",
        "4K requests are approved automatically",
    ),
    (
        Permissions::VIEW_REQUESTS,
        "VIEW_REQUESTS",
        "View requests",
        "See other users' requests",
    ),
    (
        Permissions::CREATE_ISSUES,
        "CREATE_ISSUES",
        "Report issues",
        "Report problems with available media",
    ),
    (
        Permissions::VIEW_CALENDAR,
        "VIEW_CALENDAR",
        "View calendar",
        "See the release calendar",
    ),
    (
        Permissions::VIEW_QUEUE,
        "VIEW_QUEUE",
        "View queue",
        "See the download queue",
    ),
    (
        Permissions::REQUEST_BOOK,
        "REQUEST_BOOK",
        "Request books",
        "Submit ebook and audiobook requests",
    ),
    (
        Permissions::REQUEST_MUSIC,
        "REQUEST_MUSIC",
        "Request music",
        "Submit album requests",
    ),
    (
        Permissions::AUTO_APPROVE_BOOK,
        "AUTO_APPROVE_BOOK",
        "Auto-approve books",
        "Book requests are approved automatically",
    ),
    (
        Permissions::AUTO_APPROVE_MUSIC,
        "AUTO_APPROVE_MUSIC",
        "Auto-approve music",
        "Music requests are approved automatically",
    ),
    (
        Permissions::REQUEST_RADIO,
        "REQUEST_RADIO",
        "Request radio",
        "Ask for radio programmes and podcasts to be followed",
    ),
    (
        Permissions::AUTO_APPROVE_RADIO,
        "AUTO_APPROVE_RADIO",
        "Auto-approve radio",
        "Radio requests are approved automatically",
    ),
];

pub fn roles() -> RolesDto {
    let role = |id: &str, name: &str, permissions| RolePresetDto {
        id: id.into(),
        name: name.into(),
        permissions,
    };
    RolesDto {
        roles: vec![
            role("admin", "Admin", Permissions::ROLE_ADMIN),
            role("manager", "Manager", Permissions::ROLE_MANAGER),
            role("power_user", "Power user", Permissions::ROLE_POWER_USER),
            role("user", "User", Permissions::ROLE_USER),
        ],
        permissions: PERMISSION_INFO
            .iter()
            .map(|(p, key, label, desc)| PermissionInfoDto {
                bit: p.bits(),
                key: (*key).into(),
                label: (*label).into(),
                description: (*desc).into(),
            })
            .collect(),
    }
}

fn require_users(actor: &Actor) -> Result<()> {
    if actor.permissions.has(Permissions::MANAGE_USERS) {
        Ok(())
    } else {
        Err(Error::Forbidden("managing users needs MANAGE_USERS".into()))
    }
}

fn blank(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Validates a username: 1-64 chars, no control characters or surrounding whitespace.
pub fn valid_username(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(Error::Validation(
            "username must be 1-64 printable characters".into(),
        ));
    }
    Ok(n.to_owned())
}

impl RequestService {
    pub(crate) async fn user_detail(&self, u: UserRecord) -> Result<UserDetailDto> {
        let count = self.requests_repo().count_for_user(u.id).await?;
        Ok(UserDetailDto {
            display_name: u.display(),
            id: u.id,
            username: u.username,
            email: u.email,
            avatar: u.avatar,
            auth_source: u.auth_source,
            external_id: u.external_id,
            permissions: u.permissions,
            locale: u.locale,
            movie_quota_limit: u.movie_quota_limit,
            movie_quota_days: u.movie_quota_days,
            tv_quota_limit: u.tv_quota_limit,
            tv_quota_days: u.tv_quota_days,
            book_quota_limit: u.book_quota_limit,
            book_quota_days: u.book_quota_days,
            music_quota_limit: u.music_quota_limit,
            music_quota_days: u.music_quota_days,
            has_password: u.has_password,
            request_count: count,
            created_at: ts(u.created_at),
            updated_at: ts(u.updated_at),
        })
    }

    pub async fn list_users(&self, actor: &Actor) -> Result<Vec<UserDetailDto>> {
        require_users(actor)?;
        let mut out = Vec::new();
        for u in self.users_repo().list().await? {
            out.push(self.user_detail(u).await?);
        }
        Ok(out)
    }

    pub async fn get_user(&self, actor: &Actor, id: i64) -> Result<UserDetailDto> {
        if actor.id != id {
            require_users(actor)?;
        }
        let u = self.user_record(id).await?;
        self.user_detail(u).await
    }

    /// Creates a local user. `password_hash` is computed by the caller (argon2).
    pub async fn create_user(
        &self,
        actor: &Actor,
        dto: CreateUserDto,
        password_hash: String,
    ) -> Result<UserDetailDto> {
        require_users(actor)?;
        let username = valid_username(&dto.username)?;
        let settings = self.request_settings().await?;
        let permissions = dto.permissions.unwrap_or(settings.default_permissions);
        if permissions.is_admin() && !actor.permissions.is_admin() {
            return Err(Error::Forbidden("only admins can create admins".into()));
        }
        // Explicit permissions beyond the actor's own need an admin (the defaults are fine).
        if dto.permissions.is_some()
            && !actor.permissions.is_admin()
            && !actor.permissions.contains(permissions)
        {
            return Err(Error::Forbidden(
                "you can only grant permissions your own account has".into(),
            ));
        }
        let created = self
            .db
            .users()
            .create(NewUser {
                username,
                email: blank(dto.email.clone()),
                display_name: blank(dto.display_name.clone()),
                password_hash: Some(password_hash),
                auth_source: AuthSource::Local,
                permissions,
            })
            .await
            .map_err(|e| match e {
                reelix_db::DbError::Conflict(m) => Error::Conflict(m),
                other => other.into(),
            })?;
        self.users_repo()
            .update(
                created.id,
                &UserEdit {
                    display_name: blank(dto.display_name),
                    email: blank(dto.email),
                    avatar: None,
                    locale: None,
                    permissions,
                    movie_quota_limit: dto.movie_quota_limit,
                    movie_quota_days: dto.movie_quota_days,
                    tv_quota_limit: dto.tv_quota_limit,
                    tv_quota_days: dto.tv_quota_days,
                    book_quota_limit: dto.book_quota_limit,
                    book_quota_days: dto.book_quota_days,
                    music_quota_limit: dto.music_quota_limit,
                    music_quota_days: dto.music_quota_days,
                },
            )
            .await?;
        self.events.publish(reelix_core::DomainEvent::UserCreated {
            user_id: created.id,
            username: created.username.clone(),
        });
        self.changed("user", "created", created.id, None);
        self.get_user(actor, created.id).await
    }

    /// Updates a user. A `password` change ends the user's other sessions; when someone else's
    /// password is reset, their API keys are revoked too.
    pub async fn update_user(
        &self,
        actor: &Actor,
        id: i64,
        dto: UpdateUserDto,
        password: Option<PasswordChange>,
    ) -> Result<UserDetailDto> {
        let me = actor.id == id;
        let target = self.user_record(id).await?;
        let mut permissions = dto.permissions;
        if !me || actor.permissions.has(Permissions::MANAGE_USERS) {
            require_users(actor)?;
            if !actor.permissions.is_admin() {
                if target.permissions.is_admin() && !me {
                    return Err(Error::Forbidden("only admins can edit admins".into()));
                }
                // Taking over (password) or reshaping (permissions) an account needs every
                // permission it has, or a manager could reset a higher-privileged user.
                if !me
                    && !actor.permissions.contains(target.permissions)
                    && (password.is_some() || permissions != target.permissions)
                {
                    return Err(Error::Forbidden(
                        "you can only change the password or permissions of users whose permissions your own account has"
                            .into(),
                    ));
                }
                if permissions.is_admin() && !target.permissions.is_admin() {
                    return Err(Error::Forbidden("only admins can grant ADMIN".into()));
                }
                // User managers can only hand out permissions they hold themselves.
                let added = permissions.difference(target.permissions);
                if !actor.permissions.contains(added) {
                    return Err(Error::Forbidden(
                        "you can only grant permissions your own account has".into(),
                    ));
                }
            }
        } else {
            // Plain users editing themselves keep their permissions and quotas.
            permissions = target.permissions;
        }
        if target.permissions.is_admin()
            && !permissions.is_admin()
            && self.users_repo().count_admins().await? <= 1
        {
            return Err(Error::Conflict("cannot remove ADMIN from the last admin".into()));
        }
        if password.is_some()
            && target.auth_source != "local"
            && !target.has_password
            && !actor.permissions.is_admin()
        {
            return Err(Error::Validation("external users have no local password".into()));
        }
        let quotas = if actor.permissions.has(Permissions::MANAGE_USERS) {
            [
                dto.movie_quota_limit,
                dto.movie_quota_days,
                dto.tv_quota_limit,
                dto.tv_quota_days,
                dto.book_quota_limit,
                dto.book_quota_days,
                dto.music_quota_limit,
                dto.music_quota_days,
            ]
        } else {
            [
                target.movie_quota_limit,
                target.movie_quota_days,
                target.tv_quota_limit,
                target.tv_quota_days,
                target.book_quota_limit,
                target.book_quota_days,
                target.music_quota_limit,
                target.music_quota_days,
            ]
        };
        let [mq, md, tq, td, bq, bd, uq, ud] = quotas;
        self.users_repo()
            .update(
                id,
                &UserEdit {
                    display_name: blank(dto.display_name),
                    email: blank(dto.email),
                    avatar: blank(dto.avatar),
                    locale: blank(dto.locale),
                    permissions,
                    movie_quota_limit: mq,
                    movie_quota_days: md,
                    tv_quota_limit: tq,
                    tv_quota_days: td,
                    book_quota_limit: bq,
                    book_quota_days: bd,
                    music_quota_limit: uq,
                    music_quota_days: ud,
                },
            )
            .await?;
        if let Some(change) = password {
            let repo = self.users_repo();
            repo.set_password_hash(id, &change.hash).await?;
            let keep = if me { change.keep_session.as_deref() } else { None };
            repo.delete_sessions(id, keep).await?;
            if !me {
                repo.delete_api_keys(id).await?;
            }
        }
        self.changed("user", "updated", id, Some(id));
        let u = self.user_record(id).await?;
        self.user_detail(u).await
    }

    pub async fn delete_user(&self, actor: &Actor, id: i64) -> Result<()> {
        require_users(actor)?;
        if actor.id == id {
            return Err(Error::Conflict("you cannot delete yourself".into()));
        }
        let target = self.user_record(id).await?;
        if target.permissions.is_admin() {
            if !actor.permissions.is_admin() {
                return Err(Error::Forbidden("only admins can delete admins".into()));
            }
            if self.users_repo().count_admins().await? <= 1 {
                return Err(Error::Conflict("cannot delete the last admin".into()));
            }
        }
        self.users_repo().delete(id).await?;
        self.changed("user", "deleted", id, None);
        Ok(())
    }

    // ------------------------------------------------------------ import

    pub async fn import_servers(&self, actor: &Actor) -> Result<Vec<ImportServerDto>> {
        require_users(actor)?;
        Ok(self
            .media_servers
            .servers()
            .await
            .into_iter()
            .map(|s| ImportServerDto {
                id: s.id,
                name: s.name,
                kind: kind_name(s.kind).into(),
            })
            .collect())
    }

    async fn server(&self, id: i64) -> Result<MediaServerEntry> {
        self.media_servers
            .servers()
            .await
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| Error::NotFound("media server not found".into()))
    }

    pub async fn import_preview(&self, actor: &Actor, server_id: i64) -> Result<Vec<ExternalUserDto>> {
        require_users(actor)?;
        let s = self.server(server_id).await?;
        let users = s
            .server
            .users()
            .await
            .map_err(|e| Error::Upstream(format!("{}: {e}", s.name)))?;
        let mut out = Vec::new();
        for u in users {
            let existing = self.users_repo().by_external(kind_name(s.kind), &u.id).await?;
            out.push(ExternalUserDto {
                external_id: u.id,
                username: u.username,
                email: u.email,
                is_admin: u.is_admin,
                avatar_url: u.avatar_url,
                existing_user_id: existing.map(|e| e.id),
            });
        }
        Ok(out)
    }

    pub async fn import_users(
        &self,
        actor: &Actor,
        server_id: i64,
        dto: ImportUsersDto,
    ) -> Result<ImportUsersResultDto> {
        require_users(actor)?;
        let settings = self.request_settings().await?;
        let permissions = dto.permissions.unwrap_or(settings.default_permissions);
        if permissions.is_admin() && !actor.permissions.is_admin() {
            return Err(Error::Forbidden("only admins can grant ADMIN".into()));
        }
        // As for created users: explicit permissions beyond the actor's own need an admin.
        if dto.permissions.is_some()
            && !actor.permissions.is_admin()
            && !actor.permissions.contains(permissions)
        {
            return Err(Error::Forbidden(
                "you can only grant permissions your own account has".into(),
            ));
        }
        let s = self.server(server_id).await?;
        let users = s
            .server
            .users()
            .await
            .map_err(|e| Error::Upstream(format!("{}: {e}", s.name)))?;
        let mut imported = Vec::new();
        let mut skipped = 0;
        for u in users {
            let selected = dto.external_ids.as_ref().is_none_or(|ids| ids.contains(&u.id));
            if !selected
                || self
                    .users_repo()
                    .by_external(kind_name(s.kind), &u.id)
                    .await?
                    .is_some()
            {
                skipped += 1;
                continue;
            }
            let id = self.create_external(source_of(s.kind), &u, permissions).await?;
            let rec = self.user_record(id).await?;
            imported.push(self.user_detail(rec).await?);
        }
        Ok(ImportUsersResultDto { imported, skipped })
    }

    // ------------------------------------------------------------ preferences

    pub async fn notification_prefs(&self, user_id: i64) -> Result<NotificationPrefsDto> {
        self.users_repo().prefs(user_id).await
    }

    pub async fn set_notification_prefs(
        &self,
        user_id: i64,
        mut p: NotificationPrefsDto,
    ) -> Result<NotificationPrefsDto> {
        p.events
            .retain(|e| USER_NOTIFICATION_EVENTS.iter().any(|(id, _)| id == e));
        p.events.sort();
        p.events.dedup();
        for f in [
            &mut p.email,
            &mut p.discord_user_id,
            &mut p.telegram_chat_id,
            &mut p.pushover_user_key,
        ] {
            *f = blank(f.take());
        }
        if (p.watchlist_sync_movies || p.watchlist_sync_series)
            && self.user_record(user_id).await?.plex_token.is_none()
        {
            return Err(Error::Validation(
                "watchlist sync needs an account that logged in with Plex".into(),
            ));
        }
        self.users_repo().set_prefs(user_id, &p).await?;
        self.changed("user", "updated", user_id, Some(user_id));
        Ok(p)
    }
}

pub(crate) fn kind_name(k: MediaServerKind) -> &'static str {
    match k {
        MediaServerKind::Jellyfin => "jellyfin",
        MediaServerKind::Emby => "emby",
        MediaServerKind::Plex => "plex",
        MediaServerKind::Audiobookshelf => "audiobookshelf",
        MediaServerKind::Kavita => "kavita",
        MediaServerKind::CalibreWeb => "calibreweb",
        MediaServerKind::Navidrome => "navidrome",
    }
}

pub(crate) fn source_of(k: MediaServerKind) -> AuthSource {
    match k {
        MediaServerKind::Jellyfin => AuthSource::Jellyfin,
        MediaServerKind::Emby => AuthSource::Emby,
        MediaServerKind::Plex => AuthSource::Plex,
        // Never reached: only Jellyfin/Emby/Plex servers are mapped into the requests directory.
        MediaServerKind::Audiobookshelf
        | MediaServerKind::Kavita
        | MediaServerKind::CalibreWeb
        | MediaServerKind::Navidrome => AuthSource::Local,
    }
}
