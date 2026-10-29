#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use reelix_indexers::{BuildContext, DefinitionStore};

pub fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

pub fn fixture(rel: &str) -> String {
    std::fs::read_to_string(fixture_path(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub fn store() -> Arc<DefinitionStore> {
    let (store, errors) = DefinitionStore::load_dir(&fixture_path("definitions")).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(store)
}

/// Context for tests: wiremock listens on 127.0.0.1, so private addresses are allowed.
pub fn ctx(id: i64, name: &str) -> BuildContext {
    let mut c = BuildContext::new(id, name);
    c.allow_private_addresses = true;
    c.definitions = Some(store());
    c
}
