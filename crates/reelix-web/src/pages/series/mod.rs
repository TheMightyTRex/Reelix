//! Series library screens (M5).

pub mod detail;
pub mod library;

pub use detail::SeriesDetailPage;
pub use library::SeriesPage;

use leptos::prelude::*;
use reelix_api_types::library::MediaKind;

use crate::components::library_add::AddMediaPage;
use crate::components::library_forms::require_library;
use crate::components::library_import::ImportLibraryPage;

/// `/series/add`
#[component]
pub fn AddSeriesPage() -> impl IntoView {
    require_library();
    view! { <AddMediaPage kind=MediaKind::Series /> }
}

/// `/series/import`
#[component]
pub fn ImportSeriesPage() -> impl IntoView {
    view! { <ImportLibraryPage kind=MediaKind::Series /> }
}
