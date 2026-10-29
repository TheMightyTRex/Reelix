//! Custom script notifier (unix only: uses /bin/sh scripts).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use reelix_notify::{BuildContext, MediaSummary, NotificationEvent, NotifyError, build, samples};
use serde_json::json;

fn write_script(dir: &Path, body: &str) -> String {
    let p = dir.join("hook.sh");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}

fn ctx() -> BuildContext {
    let mut c = BuildContext::new(reqwest::Client::new());
    c.timeout = Duration::from_secs(5);
    c
}

#[tokio::test]
async fn passes_event_via_env_without_shell_interpolation() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("env.txt");
    let marker = dir.path().join("pwned");
    let script = write_script(dir.path(), &format!("env > '{}'", out.display()));
    let n = build("script", &json!({ "path": script }), &ctx()).unwrap();

    let mut ev = samples::all().remove(1);
    if let NotificationEvent::Grab { media, .. } = &mut ev {
        *media = MediaSummary {
            title: format!("Evil $(touch {}) `id`", marker.display()),
            ..media.clone()
        };
    }
    n.notify(&ev).await.unwrap();

    let env = std::fs::read_to_string(&out).unwrap();
    assert!(env.contains("REELIX_EVENT_TYPE=grab"));
    assert!(env.contains("REELIX_RELEASE_QUALITY=Bluray-1080p"));
    assert!(env.contains("REELIX_MEDIA_TMDB_ID=912345"));
    assert!(env.contains("REELIX_EVENT_JSON={"));
    assert!(env.contains("REELIX_MEDIA_TITLE=Evil $(touch"));
    assert!(!marker.exists(), "title was interpolated by a shell");
    // The parent's environment (cargo sets CARGO_* for tests) must not leak.
    assert!(!env.contains("CARGO_MANIFEST_DIR"));
}

#[tokio::test]
async fn every_event_runs() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("types.txt");
    let script = write_script(
        dir.path(),
        &format!("echo \"$REELIX_EVENT_TYPE\" >> '{}'", out.display()),
    );
    let n = build("script", &json!({ "path": script }), &ctx()).unwrap();
    let events = samples::all();
    for ev in &events {
        n.notify(ev).await.unwrap();
    }
    let lines = std::fs::read_to_string(&out).unwrap();
    assert_eq!(lines.lines().count(), events.len());
    assert!(lines.lines().any(|l| l == "upgrade"));
}

#[tokio::test]
async fn arguments_failures_and_timeouts() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(dir.path(), "echo \"bad arg: $1\" >&2; exit 3");
    let n = build(
        "script",
        &json!({ "path": script, "arguments": ["--flag; rm -rf /"] }),
        &ctx(),
    )
    .unwrap();
    match n.test().await.unwrap_err() {
        NotifyError::Script(m) => assert!(m.contains("bad arg: --flag; rm -rf /"), "{m}"),
        e => panic!("unexpected {e:?}"),
    }

    let slow = write_script(dir.path(), "sleep 10");
    let mut c = ctx();
    c.timeout = Duration::from_millis(200);
    let n = build("script", &json!({ "path": slow }), &c).unwrap();
    match n.test().await.unwrap_err() {
        NotifyError::Script(m) => assert!(m.contains("timed out")),
        e => panic!("unexpected {e:?}"),
    }

    assert!(build("script", &json!({ "path": "relative/hook.sh" }), &ctx()).is_err());
    let missing = build("script", &json!({ "path": "/nonexistent/reelix-hook" }), &ctx()).unwrap();
    assert!(matches!(missing.test().await, Err(NotifyError::Script(_))));
}
