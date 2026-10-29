//! `/indexers`, `/indexers/search`, `/indexers/stats`.

pub mod list;
pub mod search;
pub mod stats;

pub use list::IndexersPage;
pub use search::IndexerSearchPage;
pub use stats::IndexerStatsPage;
