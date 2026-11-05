//! Provider persistence and live instances: indexers, download clients, notifications,
//! media servers, import lists, metadata clients, tags and remote path mappings.
//!
//! [`ProviderService`] is the single entry point. Rows live in the `providers` table with
//! every `Secret` settings field encrypted via `SecretBox`; live adapter instances are
//! cached and rebuilt whenever a row changes.

pub mod crypto;
pub mod definitions;
mod error;
pub mod jobs;
pub mod model;
mod repo;
pub mod schemas;
mod service;

pub use error::{ProviderError, Result};
pub use model::{ProviderRow, RemotePathMapping, Tag, apply_mappings};
pub use reelix_api_types::providers::ProviderKind;
pub use service::{
    DEFINITIONS_SETTINGS_KEY, DefinitionsSyncOutcome, INDEXER_SETTINGS_KEY, METADATA_SETTINGS_KEY,
    ProviderOptions, ProviderService, public_http_client,
};
