//! `/settings/notifications` — notification targets with per-event subscriptions.

use leptos::prelude::*;
use reelix_api_types::providers::ProviderKind;

use crate::components::PageHeader;
use crate::components::provider_form::ProviderListCard;
use crate::pages::system::require_admin;

#[component]
pub fn NotificationsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Notifications" subtitle="Discord, Slack, Telegram, email, webhooks and more. Pick which events each target receives." />
        <ProviderListCard kind=ProviderKind::Notification title="Connections" />
    }
}
