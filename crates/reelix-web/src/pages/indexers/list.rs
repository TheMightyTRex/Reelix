//! `/indexers` — configured indexers with status, priority and protocol, plus the add catalog.

use leptos::prelude::*;
use leptos_router::components::A;
use reelix_api_types::providers::ProviderKind;

use crate::components::PageHeader;
use crate::components::provider_form::ProviderListCard;
use crate::pages::system::require_admin;

#[component]
pub fn IndexersPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Indexers" subtitle="Torznab, Newznab and YAML definitions. Failing indexers back off automatically and are re-tested every 15 minutes.">
            <A href="/settings/indexers" attr:class="btn btn-secondary btn-sm">"Indexer settings"</A>
        </PageHeader>
        <ProviderListCard kind=ProviderKind::Indexer title="Indexers"
            description="Lower priority numbers are preferred when releases are otherwise equal." />
    }
}
