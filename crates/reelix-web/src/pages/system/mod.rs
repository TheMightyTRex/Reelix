//! System screens.

pub mod events;
pub mod health;
pub mod status;
pub mod tasks;

pub use events::EventsPage;
pub use health::HealthPage;
pub use status::StatusPage;
pub use tasks::TasksPage;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::state::AuthState;

/// Sends non-admins away from admin-only pages.
pub(crate) fn require_admin() {
    let auth = AuthState::expect();
    let navigate = use_navigate();
    Effect::new(move |_| {
        if auth.is_logged_in() && !auth.is_admin() {
            navigate("/discover", Default::default());
        }
    });
}
