//! Who is signed in, and whether the server still needs its first-run setup.

use leptos::prelude::*;
use reelix_api_types::{Permissions, SetupStatus, UserDto};

use crate::api;

/// Outcome of the boot sequence (`GET /setup`, then `GET /auth/me`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// Still talking to the server.
    Pending,
    /// No admin exists yet; the setup wizard must run.
    NeedsSetup,
    /// Setup is done; `user` says whether someone is signed in.
    Ready,
    /// The server could not be reached.
    Offline,
}

#[derive(Clone, Copy)]
pub struct AuthState {
    pub user: RwSignal<Option<UserDto>>,
    pub boot: RwSignal<Boot>,
    /// Server version reported by `GET /setup`.
    pub server_version: RwSignal<Option<String>>,
}

impl AuthState {
    pub fn new() -> Self {
        Self {
            user: RwSignal::new(None),
            boot: RwSignal::new(Boot::Pending),
            server_version: RwSignal::new(None),
        }
    }

    pub fn expect() -> Self {
        expect_context()
    }

    pub fn is_logged_in(&self) -> bool {
        self.user.with(Option::is_some)
    }

    /// Whether the signed-in user holds `required` (admins hold everything).
    pub fn has(&self, required: Permissions) -> bool {
        self.user
            .with(|u| u.as_ref().is_some_and(|u| u.permissions.has(required)))
    }

    /// Whether the signed-in user holds at least one of `any` (admins hold everything).
    pub fn has_any(&self, any: Permissions) -> bool {
        self.user
            .with(|u| u.as_ref().is_some_and(|u| u.permissions.has_any(any)))
    }

    pub fn is_admin(&self) -> bool {
        self.user
            .with(|u| u.as_ref().is_some_and(|u| u.permissions.is_admin()))
    }

    pub fn sign_in(&self, user: UserDto) {
        self.user.set(Some(user));
        self.boot.set(Boot::Ready);
    }

    pub fn clear(&self) {
        if self.user.with_untracked(Option::is_some) {
            self.user.set(None);
        }
    }

    /// Runs the boot sequence.
    pub async fn boot(self) {
        self.boot.set(Boot::Pending);
        match api::get::<SetupStatus>("/setup").await {
            Ok(status) => {
                self.server_version.set(Some(status.version));
                if status.needs_setup {
                    self.user.set(None);
                    self.boot.set(Boot::NeedsSetup);
                    return;
                }
            }
            Err(_) => {
                self.boot.set(Boot::Offline);
                return;
            }
        }
        match api::get::<UserDto>("/auth/me").await {
            Ok(user) => self.user.set(Some(user)),
            Err(_) => self.user.set(None),
        }
        self.boot.set(Boot::Ready);
    }
}

/// Short role name derived from a permission set.
pub fn role_name(p: Permissions) -> &'static str {
    if p.is_admin() {
        "Administrator"
    } else if p.contains(Permissions::ROLE_MANAGER) {
        "Manager"
    } else if p.contains(Permissions::ROLE_POWER_USER) {
        "Power user"
    } else {
        "User"
    }
}
