//! Activity screens (M4): download queue (+ pending releases), history and blocklist.

pub mod blocklist;
pub mod history;
pub mod queue;

pub use blocklist::BlocklistPage;
pub use history::HistoryPage;
pub use queue::{ActiveDownloads, QueuePage};
