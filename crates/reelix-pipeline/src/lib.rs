//! The search → decision → grab → download tracking → import pipeline (M4).
//!
//! `store` holds the persistence shared by every stage; stage modules are added alongside it.

pub mod import;
pub mod manual;
pub mod monitor;
pub mod search;
pub mod store;

pub use monitor::{ActivityError, ActivityProviders, ActivityService, ClientHandle, StaticProviders};
pub use store::{FormatRef, GrabbedRelease};
