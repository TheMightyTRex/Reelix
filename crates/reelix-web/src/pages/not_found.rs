//! 404 page for unknown addresses inside the app shell.

use leptos::prelude::*;
use leptos_router::components::A;

use crate::components::{EmptyState, Icon};

#[component]
pub fn NotFound() -> impl IntoView {
    view! {
        <div class="card not-found">
            <EmptyState icon="discover" title="Page not found" description="There's nothing at this address.">
                <A href="/" attr:class="btn btn-primary">
                    <Icon name="dashboard" />
                    "Back home"
                </A>
            </EmptyState>
        </div>
    }
}
