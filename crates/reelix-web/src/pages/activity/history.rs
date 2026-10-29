//! `/activity/history` — every grab, import, failure, deletion and rename.

use leptos::prelude::*;

use crate::components::PageHeader;
use crate::components::history_table::{EventTypeSelect, HistoryTable};
use crate::components::library_forms::require_library;

#[component]
pub fn HistoryPage() -> impl IntoView {
    require_library();
    let event_type = RwSignal::new(String::new());
    view! {
        <PageHeader title="History" subtitle="Grabs, imports, upgrades and failures across the library.">
            <EventTypeSelect value=event_type id="hist-event" />
        </PageHeader>
        <div class="card">
            <div class="card-body">
                <HistoryTable event_type=event_type page_size=50 />
            </div>
        </div>
    }
}
