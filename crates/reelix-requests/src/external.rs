//! External login: Jellyfin / Emby (username + password against a media server), Plex
//! (plex.tv PIN flow, owner or friend of a configured Plex server) and the shared
//! find-or-create logic (OIDC lives in [`crate::oidc`]).

use reelix_api_types::requests::{AuthMethodsDto, PlexPinDto};
use reelix_core::{AuthSource, Permissions, User};
use reelix_db::NewUser;
use reelix_mediaserver::{ExternalUser, MediaServerKind, MsError};

use crate::users::kind_name;
use crate::{Error, RequestService, Result};

impl RequestService {
    /// Which login buttons the login page shows (public).
    pub async fn auth_methods(&self) -> Result<AuthMethodsDto> {
        let s = self.security_settings().await?;
        let oidc = s.oidc.enabled && !s.oidc.issuer_url.is_empty() && !s.oidc.client_id.is_empty();
        Ok(AuthMethodsDto {
            local: s.local_login,
            jellyfin: s.jellyfin_login,
            emby: s.emby_login,
            plex: s.plex_login,
            oidc,
            oidc_label: oidc.then(|| s.oidc.button_label.clone()),
        })
    }

    /// Creates a user for an external identity with a unique username.
    pub(crate) async fn create_external(
        &self,
        source: AuthSource,
        ext: &ExternalUser,
        permissions: Permissions,
    ) -> Result<i64> {
        let base = crate::users::valid_username(&ext.username)
            .or_else(|_| crate::users::valid_username(ext.email.as_deref().unwrap_or("")))
            .unwrap_or_else(|_| format!("{}-{}", source.as_str(), ext.id));
        let mut name = base.clone();
        let mut n = 1;
        while self.users_repo().username_taken(&name).await? {
            n += 1;
            name = format!("{base}-{n}");
        }
        let u = self
            .db
            .users()
            .create(NewUser {
                username: name,
                email: ext.email.clone(),
                display_name: Some(ext.username.clone()).filter(|s| !s.trim().is_empty()),
                password_hash: None,
                auth_source: source,
                permissions,
            })
            .await
            .map_err(|e| match e {
                reelix_db::DbError::Conflict(m) => Error::Conflict(m),
                other => other.into(),
            })?;
        self.users_repo()
            .link_external(u.id, &ext.id, ext.email.as_deref(), ext.avatar_url.as_deref())
            .await?;
        self.events.publish(reelix_core::DomainEvent::UserCreated {
            user_id: u.id,
            username: u.username,
        });
        self.changed("user", "created", u.id, None);
        Ok(u.id)
    }

    /// Finds the user linked to `ext` or creates one (when `auto_create`).
    pub(crate) async fn find_or_create(
        &self,
        source: AuthSource,
        ext: &ExternalUser,
        auto_create: bool,
    ) -> Result<User> {
        let id = match self.users_repo().by_external(source.as_str(), &ext.id).await? {
            Some(u) => {
                self.users_repo()
                    .link_external(u.id, &ext.id, ext.email.as_deref(), ext.avatar_url.as_deref())
                    .await?;
                u.id
            }
            None if auto_create => {
                let perms = self.request_settings().await?.default_permissions;
                self.create_external(source, ext, perms).await?
            }
            None => {
                return Err(Error::Unauthorized(
                    "no Reelix account is linked to this login; ask an admin to import you".into(),
                ));
            }
        };
        self.db
            .users()
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound("user not found".into()))
    }

    /// Jellyfin / Emby login. Returns the Reelix user to start a session for.
    pub async fn media_server_login(
        &self,
        kind: MediaServerKind,
        username: &str,
        password: &str,
    ) -> Result<User> {
        let s = self.security_settings().await?;
        let (enabled, wanted) = match kind {
            MediaServerKind::Jellyfin => (s.jellyfin_login, s.jellyfin_server_id),
            MediaServerKind::Emby => (s.emby_login, s.emby_server_id),
            MediaServerKind::Plex
            | MediaServerKind::Audiobookshelf
            | MediaServerKind::Kavita
            | MediaServerKind::CalibreWeb
            | MediaServerKind::Navidrome => (false, None),
        };
        if !enabled {
            return Err(Error::Forbidden(format!("{} login is disabled", kind_name(kind))));
        }
        let servers = self.media_servers.servers().await;
        let server = servers
            .iter()
            .filter(|e| e.kind == kind)
            .find(|e| wanted.is_none_or(|w| w == e.id))
            .ok_or_else(|| Error::Upstream(format!("no {} server is configured", kind_name(kind))))?;
        let ext = match server.server.authenticate(username.trim(), password).await {
            Ok(u) => u,
            Err(MsError::InvalidCredentials | MsError::Unauthorized) => {
                return Err(Error::Unauthorized("invalid username or password".into()));
            }
            Err(e) => return Err(Error::Upstream(format!("{}: {e}", server.name))),
        };
        let source = crate::users::source_of(kind);
        self.find_or_create(source, &ext, s.media_server_auto_create)
            .await
    }

    /// Starts a Plex PIN login.
    pub async fn plex_create_pin(&self, forward_url: Option<&str>) -> Result<PlexPinDto> {
        if !self.security_settings().await?.plex_login {
            return Err(Error::Forbidden("plex login is disabled".into()));
        }
        let pin = self
            .media_servers
            .plex_tv()
            .create_pin(forward_url)
            .await
            .map_err(|e| Error::Upstream(format!("plex.tv: {e}")))?;
        Ok(PlexPinDto {
            id: pin.id,
            code: pin.code,
            auth_url: pin.auth_url,
        })
    }

    /// Polls a Plex PIN. `Ok(None)` = not approved yet. On success the account must own or
    /// be a friend of a configured Plex server.
    pub async fn plex_check_pin(&self, pin_id: i64) -> Result<Option<User>> {
        let s = self.security_settings().await?;
        if !s.plex_login {
            return Err(Error::Forbidden("plex login is disabled".into()));
        }
        let plex = self.media_servers.plex_tv();
        let Some(token) = plex
            .check_pin(pin_id)
            .await
            .map_err(|e| Error::Upstream(format!("plex.tv: {e}")))?
        else {
            return Ok(None);
        };
        let account = plex
            .account(&token)
            .await
            .map_err(|e| Error::Upstream(format!("plex.tv: {e}")))?;
        let mut member = false;
        for e in self.media_servers.servers().await {
            if e.kind != MediaServerKind::Plex {
                continue;
            }
            match e.server.users().await {
                Ok(users) if users.iter().any(|u| u.id == account.id) => {
                    member = true;
                    break;
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(server = %e.name, error = %err, "could not list Plex server users")
                }
            }
        }
        if !member {
            return Err(Error::Forbidden(
                "this Plex account has no access to a configured Plex server".into(),
            ));
        }
        let user = self
            .find_or_create(AuthSource::Plex, &account, s.media_server_auto_create)
            .await?;
        let enc = self.secrets.encrypt(&token)?;
        self.users_repo().set_plex_token(user.id, Some(&enc)).await?;
        Ok(Some(user))
    }
}
