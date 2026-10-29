use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Permissions;

/// Where a user's identity comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthSource {
    Local,
    Jellyfin,
    Emby,
    Plex,
    Oidc,
}

impl AuthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Jellyfin => "jellyfin",
            Self::Emby => "emby",
            Self::Plex => "plex",
            Self::Oidc => "oidc",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "local" => Self::Local,
            "jellyfin" => Self::Jellyfin,
            "emby" => Self::Emby,
            "plex" => Self::Plex,
            "oidc" => Self::Oidc,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub auth_source: AuthSource,
    pub permissions: Permissions,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }
}
