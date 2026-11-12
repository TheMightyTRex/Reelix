//! Radio providers, one module each. Register new ones in the server's radio wiring
//! (`crates/reelix-server/src/modules.rs`) before [`rss`], which accepts any http(s) link.
//!
//! - [`bbc`]: BBC programmes and BBC Sounds search (undocumented public endpoints).
//! - [`apple`]: Apple Podcasts search; hands shows to [`rss`].
//! - [`podcastindex`]: Podcast Index (needs an API key and secret).
//! - [`archive`]: radio items of the Internet Archive.
//! - [`nts`], [`mixcloud`], [`globalplayer`]: NTS, Mixcloud and Global Player, off by default; audio only through
//!   catch-up ([`crate::catchup`]).
//! - [`rss`]: any podcast feed.

pub mod apple;
pub mod archive;
pub mod bbc;
pub mod globalplayer;
pub mod http;
pub mod mixcloud;
pub mod nts;
pub mod podcastindex;
pub mod rss;

pub use apple::AppleProvider;
pub use archive::ArchiveProvider;
pub use bbc::BbcProvider;
pub use globalplayer::GlobalPlayerProvider;
pub use mixcloud::MixcloudProvider;
pub use nts::NtsProvider;
pub use podcastindex::PodcastIndexProvider;
pub use rss::RssProvider;
