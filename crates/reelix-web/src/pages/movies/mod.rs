//! Movie library screens (M2).

pub mod collections;
pub mod detail;
pub mod library;

pub use collections::CollectionsPage;
pub use detail::MovieDetailPage;
pub use library::MoviesPage;

use leptos::prelude::*;
use reelix_api_types::library::MediaKind;

use crate::components::library_add::AddMediaPage;
use crate::components::library_forms::require_library;
use crate::components::library_import::ImportLibraryPage;

/// `/movies/add`
#[component]
pub fn AddMoviePage() -> impl IntoView {
    require_library();
    view! { <AddMediaPage kind=MediaKind::Movie /> }
}

/// `/movies/import`
#[component]
pub fn ImportMoviesPage() -> impl IntoView {
    view! { <ImportLibraryPage kind=MediaKind::Movie /> }
}
