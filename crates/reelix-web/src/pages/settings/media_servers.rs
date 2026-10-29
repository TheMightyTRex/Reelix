//! `/settings/mediaservers` — Jellyfin, Emby and Plex (with plex.tv sign-in).

use leptos::prelude::*;
use reelix_api_types::providers::ProviderKind;

use crate::components::PageHeader;
use crate::components::provider_form::ProviderListCard;
use crate::pages::system::require_admin;

#[component]
pub fn MediaServersPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Media Servers" subtitle="Library refresh after imports, availability for requests and user import." />
        <ProviderListCard kind=ProviderKind::MediaServer title="Servers"
            description="For Plex, use “Sign in with Plex” in the form to fetch a token." />
    }
}
