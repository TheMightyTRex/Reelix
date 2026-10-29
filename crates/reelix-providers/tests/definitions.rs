//! Indexer definition catalog sync against wiremock (no real network). The archive is built
//! in the test from ORIGINAL fictional definitions.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use reelix_api_types::providers::{
    DefinitionSource, DefinitionsSettingsDto, IndexerSettingsDto, ProviderInput, ProviderKind,
};
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_providers::{DefinitionsSyncOutcome, ProviderOptions, ProviderService};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A minimal, fictional public JSON tracker definition.
fn def_yaml(id: &str, name: &str) -> String {
    format!(
        r#"# Fictional definition for the Reelix test-suite.
id: {id}
name: {name}
description: Fictional catalog entry
language: en-US
type: public
links:
  - https://{id}.invalid/
caps:
  categories:
    movies: Movies
  modes:
    search: [q]
search:
  paths:
    - path: api/list
      response:
        type: json
  inputs:
    q: "{{{{ .Keywords }}}}"
  rows:
    selector: items
  fields:
    title:
      selector: title
    infohash:
      selector: hash
    size:
      selector: bytes
"#
    )
}

fn append_file(b: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    // Write the raw name (bypasses `set_path`'s `..` check so we can craft hostile entries).
    let name = &mut h.as_old_mut().name;
    name.fill(0);
    name[..path.len()].copy_from_slice(path.as_bytes());
    h.set_cksum();
    b.append(&h, data).unwrap();
}

/// GitHub-like archive: pax global header with the commit, two v11 definitions, one broken
/// v11 file, a v10 file, a README and a path-traversal entry.
fn archive(extra: &[(&str, String)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let record = format!("52 comment={COMMIT}\n");
    let mut h = tar::Header::new_ustar();
    h.set_path("pax_global_header").unwrap();
    h.set_size(record.len() as u64);
    h.set_entry_type(tar::EntryType::XGlobalHeader);
    h.set_cksum();
    b.append(&h, record.as_bytes()).unwrap();
    append_file(&mut b, "Indexers-master/README.md", b"# fictional");
    append_file(
        &mut b,
        "Indexers-master/definitions/v11/alpha.yml",
        def_yaml("alpha", "Alpha Fictional").as_bytes(),
    );
    append_file(
        &mut b,
        "Indexers-master/definitions/v11/bravo.yml",
        def_yaml("bravo", "Bravo Fictional").as_bytes(),
    );
    append_file(
        &mut b,
        "Indexers-master/definitions/v11/broken.yml",
        b"id: broken\nname: [",
    );
    append_file(
        &mut b,
        "Indexers-master/definitions/v10/charlie.yml",
        def_yaml("charlie", "Charlie Old Schema").as_bytes(),
    );
    append_file(
        &mut b,
        "Indexers-master/definitions/v11/../../../evil.yml",
        def_yaml("evil", "Evil").as_bytes(),
    );
    for (p, data) in extra {
        append_file(&mut b, p, data.as_bytes());
    }
    let tar = b.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

struct Env {
    svc: Arc<ProviderService>,
    events: EventBus,
    dir: tempfile::TempDir,
}

impl Env {
    fn defs(&self) -> std::path::PathBuf {
        self.dir.path().join("definitions")
    }
}

async fn env_in(dir: tempfile::TempDir) -> Env {
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let svc = ProviderService::new(
        db,
        SecretBox::from_key([5; 32]),
        events.clone(),
        reqwest::Client::new(),
        ProviderOptions::new(dir.path()),
    )
    .await
    .unwrap();
    Env { svc, events, dir }
}

async fn env() -> Env {
    env_in(tempfile::tempdir().unwrap()).await
}

async fn point_to(svc: &ProviderService, url: String) {
    svc.set_definitions_settings(DefinitionsSettingsDto {
        source_url: url,
        ..Default::default()
    })
    .await
    .unwrap();
}

fn gz(body: Vec<u8>) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("etag", "\"v1\"")
        .set_body_raw(body, "application/x-gzip")
}

fn has_schema(svc: &ProviderService, implementation: &str) -> Option<String> {
    svc.schemas(ProviderKind::Indexer)
        .into_iter()
        .find(|s| s.schema.implementation == implementation)
        .map(|s| s.schema.name)
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn sync_extracts_safely_and_reloads_live() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalog.tar.gz"))
        .respond_with(gz(archive(&[])))
        .mount(&server)
        .await;
    let e = env().await;
    let mut rx = e.events.subscribe();
    assert!(e.svc.definitions_sync_due(), "empty upstream → due");
    assert!(has_schema(&e.svc, "definition:alpha").is_none());
    point_to(&e.svc, format!("{}/catalog.tar.gz", server.uri())).await;

    // An existing Torznab indexer must keep its instance across the definition reload.
    e.svc
        .set_indexer_settings(IndexerSettingsDto {
            allow_private_addresses: true,
            ..Default::default()
        })
        .await
        .unwrap();
    e.svc
        .create(
            ProviderKind::Indexer,
            &ProviderInput {
                implementation: "torznab".into(),
                name: "Generic".into(),
                enabled: true,
                priority: 25,
                tags: vec![],
                settings: json!({"base_url": server.uri(), "api_key": "k"}),
            },
        )
        .await
        .unwrap();
    let before = e.svc.indexers()[0].1.clone();

    let out = e.svc.sync_definitions(true).await.unwrap();
    assert_eq!(out, DefinitionsSyncOutcome::Updated(3));
    let upstream = e.defs().join("upstream");
    assert_eq!(files(&upstream), ["alpha.yml", "bravo.yml", "broken.yml"]);
    // Traversal entry and other schema versions are ignored; no temp dirs left behind.
    assert!(!e.dir.path().join("evil.yml").exists());
    assert!(!e.dir.path().parent().unwrap().join("evil.yml").exists());
    assert!(!e.defs().join("evil.yml").exists());
    assert_eq!(files(&e.defs()), ["custom", "upstream"]);

    // Live reload: new schemas are available without a restart.
    assert_eq!(
        has_schema(&e.svc, "definition:alpha").as_deref(),
        Some("Alpha Fictional")
    );
    assert!(has_schema(&e.svc, "definition:bravo").is_some());
    assert!(has_schema(&e.svc, "definition:charlie").is_none());
    let after = e.svc.indexers()[0].1.clone();
    assert!(Arc::ptr_eq(&before, &after), "torznab instance untouched");

    let st = e.svc.definitions_status();
    assert_eq!(st.upstream_count, 3);
    assert_eq!(st.custom_count, 0);
    assert_eq!(st.loaded, 2);
    assert_eq!(st.failed.len(), 1);
    assert_eq!(st.failed[0].id, "broken");
    assert_eq!(st.failed[0].file, "broken.yml");
    assert_eq!(st.failed[0].source, DefinitionSource::Upstream);
    assert_eq!(st.commit.as_deref(), Some(COMMIT));
    assert!(st.last_sync.is_some() && st.last_error.is_none() && !st.syncing);
    let settings = e.svc.definitions_settings();
    assert_eq!(settings.etag.as_deref(), Some("\"v1\""));
    assert_eq!(settings.upstream_count, 3);

    let mut synced = false;
    while let Ok(ev) = rx.try_recv() {
        if let DomainEvent::EntityChanged { entity, action, .. } = ev
            && entity == "indexer_definitions"
            && action == "synced"
        {
            synced = true;
        }
    }
    assert!(synced, "EntityChanged indexer_definitions/synced published");

    // Not due again until the interval elapses.
    assert!(!e.svc.definitions_sync_due());
    assert_eq!(
        e.svc.sync_definitions(false).await.unwrap(),
        DefinitionsSyncOutcome::Skipped
    );
}

#[tokio::test]
async fn not_modified_keeps_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/c.tgz"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/c.tgz"))
        .respond_with(gz(archive(&[])))
        .with_priority(5)
        .expect(1)
        .mount(&server)
        .await;
    let e = env().await;
    point_to(&e.svc, format!("{}/c.tgz", server.uri())).await;
    assert!(matches!(
        e.svc.sync_definitions(true).await.unwrap(),
        DefinitionsSyncOutcome::Updated(_)
    ));
    let first = e.svc.definitions_status().last_sync;
    assert_eq!(
        e.svc.sync_definitions(true).await.unwrap(),
        DefinitionsSyncOutcome::UpToDate
    );
    let st = e.svc.definitions_status();
    assert_eq!(st.last_sync, first, "304 does not count as a new sync");
    assert!(st.last_check >= first);
    assert_eq!(st.loaded, 2);
}

#[tokio::test]
async fn custom_overrides_upstream_and_loose_files_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let defs = dir.path().join("definitions");
    std::fs::create_dir_all(&defs).unwrap();
    // A loose file from before the upstream/custom split.
    std::fs::write(defs.join("alpha.yml"), def_yaml("alpha", "Alpha Custom")).unwrap();
    std::fs::write(defs.join("delta.yml"), def_yaml("delta", "Delta Custom")).unwrap();
    let e = env_in(dir).await;
    assert_eq!(files(&e.defs().join("custom")), ["alpha.yml", "delta.yml"]);
    assert_eq!(files(&e.defs()), ["custom", "upstream"]);
    assert_eq!(
        has_schema(&e.svc, "definition:delta").as_deref(),
        Some("Delta Custom")
    );

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(gz(archive(&[])))
        .mount(&server)
        .await;
    point_to(&e.svc, format!("{}/c.tgz", server.uri())).await;
    e.svc.sync_definitions(true).await.unwrap();
    assert_eq!(
        has_schema(&e.svc, "definition:alpha").as_deref(),
        Some("Alpha Custom")
    );
    assert_eq!(
        has_schema(&e.svc, "definition:bravo").as_deref(),
        Some("Bravo Fictional")
    );
    let st = e.svc.definitions_status();
    assert_eq!(
        (st.custom_count, st.upstream_count, st.overridden, st.loaded),
        (2, 3, 1, 3)
    );
    assert!(st.custom_dir.ends_with("custom"));

    // Removing the custom file falls back to upstream after a reload.
    std::fs::remove_file(e.defs().join("custom/alpha.yml")).unwrap();
    e.svc.reload_definitions().await.unwrap();
    assert_eq!(
        has_schema(&e.svc, "definition:alpha").as_deref(),
        Some("Alpha Fictional")
    );
}

#[tokio::test]
async fn failures_keep_previous_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/good"))
        .respond_with(gz(archive(&[])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/empty"))
        .respond_with({
            // Valid archive, but nothing under definitions/v11.
            let mut b = tar::Builder::new(Vec::new());
            append_file(&mut b, "x/definitions/v10/a.yml", def_yaml("a", "A").as_bytes());
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&b.into_inner().unwrap()).unwrap();
            ResponseTemplate::new(200).set_body_raw(gz.finish().unwrap(), "application/x-gzip")
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/garbage"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"not a tarball".to_vec(), "application/x-gzip"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/down"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let e = env().await;
    point_to(&e.svc, format!("{}/good", server.uri())).await;
    e.svc.sync_definitions(true).await.unwrap();
    for p in ["/empty", "/garbage", "/down"] {
        point_to(&e.svc, format!("{}{p}", server.uri())).await;
        assert!(e.svc.sync_definitions(true).await.is_err(), "{p}");
        let st = e.svc.definitions_status();
        assert!(st.last_error.is_some(), "{p}");
        assert_eq!(st.loaded, 2, "{p}: previous catalog kept");
        assert_eq!(files(&e.defs()), ["custom", "upstream"], "{p}: no temp dirs left");
    }
    assert!(
        e.svc
            .health()
            .iter()
            .any(|h| h.message.contains("could not be updated"))
    );
}

#[tokio::test]
async fn settings_validation() {
    let e = env().await;
    let s = e.svc.definitions_settings();
    assert!(s.enabled);
    assert_eq!(s.schema_version, "v11");
    assert_eq!(s.auto_update_hours, 24);
    assert!(s.source_url.starts_with("https://codeload.github.com/"));
    for bad in [
        DefinitionsSettingsDto {
            source_url: "file:///etc/passwd".into(),
            ..Default::default()
        },
        DefinitionsSettingsDto {
            schema_version: "../v11".into(),
            ..Default::default()
        },
        DefinitionsSettingsDto {
            auto_update_hours: 0,
            ..Default::default()
        },
    ] {
        assert!(e.svc.set_definitions_settings(bad).await.is_err());
    }
    let saved = e
        .svc
        .set_definitions_settings(DefinitionsSettingsDto {
            enabled: false,
            auto_update_hours: 48,
            // Read-only state is ignored on save.
            last_error: Some("x".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!saved.enabled && saved.auto_update_hours == 48 && saved.last_error.is_none());
    assert!(!e.svc.definitions_sync_due(), "auto-update disabled");
}
