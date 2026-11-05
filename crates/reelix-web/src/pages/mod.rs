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
pub mod not_found;
pub mod profile;
pub mod radio;
pub mod requests;
pub mod search;
pub mod series;
pub mod settings;
pub mod setup;
pub mod system;
pub mod wanted;

pub use dashboard::DashboardPage;
pub use login::LoginPage;
pub use not_found::NotFound;
pub use profile::ProfilePage;
pub use setup::SetupPage;

pub mod downloads;
