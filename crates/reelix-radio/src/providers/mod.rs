//! Radio providers, one module each. Register new ones in the server's radio wiring
//! (`crates/reelix-server/src/modules.rs`) before [`rss`], which accepts any http(s) link.

pub mod rss;

pub use rss::RssProvider;
