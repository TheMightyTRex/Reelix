//! Shared helpers for adapter integration tests.
#![allow(dead_code)]

use reelix_downloaders::{BuildContext, DownloadClient};
use serde_json::Value;
use wiremock::{MockServer, ResponseTemplate};

/// Reads `tests/fixtures/<path>`.
pub fn fixture(path: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("fixture {}: {e}", p.display()))
}

/// JSON fixture response.
pub fn json_fixture(path: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/json")
        .set_body_string(fixture(path))
}

/// Host/port settings pointing at the mock server merged into `extra`.
pub fn settings_for(server: &MockServer, extra: Value) -> Value {
    let addr = server.address();
    let mut v = serde_json::json!({ "host": addr.ip().to_string(), "port": addr.port() });
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    v
}

pub fn build(implementation: &str, settings: Value) -> Box<dyn DownloadClient> {
    reelix_downloaders::build(
        implementation,
        &settings,
        &BuildContext::new(reqwest::Client::new()),
    )
    .unwrap_or_else(|e| panic!("build {implementation}: {e}"))
}
