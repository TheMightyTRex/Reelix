//! Routed screens.

pub mod activity;
pub mod books;
pub mod calendar;
pub mod dashboard;
pub mod discover;
pub mod indexers;
pub mod issues;
pub mod login;
pub mod media;
pub mod movies;
pub mod music;
pub mod placeholder;
pub mod profile;
pub mod requests;
pub mod search;
pub mod series;
pub mod settings;
pub mod setup;
pub mod system;
pub mod wanted;

pub use dashboard::DashboardPage;
pub use login::LoginPage;
pub use placeholder::{NotFound, PlaceholderPage};
pub use profile::ProfilePage;
pub use setup::SetupPage;
