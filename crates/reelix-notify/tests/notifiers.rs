//! Notifier adapters against mocked HTTP services.

use reelix_notify::{BuildContext, NotificationEvent, Notifier, NotifyError, build, samples, schemas};
use serde_json::{Value, json};
use wiremock::matchers::{
    body_partial_json, body_string_contains, header, header_exists, method, path, query_param,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn ctx() -> BuildContext {
    BuildContext::new(reqwest::Client::new())
}

fn grab() -> NotificationEvent {
    samples::all().remove(1)
}

fn notifier(implementation: &str, settings: Value) -> Box<dyn Notifier> {
    build(implementation, &settings, &ctx()).unwrap()
}

#[tokio::test]
async fn discord_posts_embed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/webhooks/1/tok"))
        .and(query_param("wait", "true"))
        .and(body_partial_json(json!({
            "username": "Reelix",
            "embeds": [{
                "title": "Grabbed: The Lighthouse Keeper (2023)",
                "url": "https://www.themoviedb.org/movie/912345",
                "thumbnail": { "url": "https://image.tmdb.org/t/p/w500/lighthouse.jpg" }
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("discord_message.json")))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        "discord",
        json!({ "webhook_url": format!("{}/api/webhooks/1/tok", server.uri()) }),
    );
    n.notify(&grab()).await.unwrap();
}

#[tokio::test]
async fn discord_rate_limit_is_typed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "2")
                .set_body_json(fixture("discord_rate_limited.json")),
        )
        .mount(&server)
        .await;
    let n = notifier("discord", json!({ "webhook_url": server.uri() }));
    let err = n.test().await.unwrap_err();
    assert!(matches!(
        err,
        NotifyError::RateLimited {
            retry_after_secs: Some(2)
        }
    ));
}

#[tokio::test]
async fn slack_posts_blocks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/T0/B0/x"))
        .and(body_partial_json(json!({
            "text": "Grabbed: The Lighthouse Keeper (2023)",
            "icon_emoji": ":movie_camera:",
            "blocks": [{ "type": "header" }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        "slack",
        json!({ "webhook_url": format!("{}/services/T0/B0/x", server.uri()), "icon": ":movie_camera:" }),
    );
    n.notify(&grab()).await.unwrap();
}

#[tokio::test]
async fn slack_invalid_webhook() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string("no_service"))
        .mount(&server)
        .await;
    let n = notifier("slack", json!({ "webhook_url": server.uri() }));
    match n.test().await.unwrap_err() {
        NotifyError::Status { status, body } => {
            assert_eq!(status, 404);
            assert_eq!(body, "no_service");
        }
        e => panic!("unexpected {e:?}"),
    }
}

#[tokio::test]
async fn telegram_sends_html() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bot123:ABC/sendMessage"))
        .and(body_partial_json(json!({
            "chat_id": "-1001234567890",
            "parse_mode": "HTML",
            "disable_notification": true,
            "message_thread_id": 9
        })))
        .and(body_string_contains(
            "<b>Grabbed: The Lighthouse Keeper (2023)</b>",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("telegram_ok.json")))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        "telegram",
        json!({ "bot_token": "123:ABC", "chat_id": "-1001234567890", "silent": true,
                "topic_id": 9, "api_url": server.uri() }),
    );
    n.notify(&grab()).await.unwrap();
}

#[tokio::test]
async fn telegram_errors() {
    let server = MockServer::start().await;
    Mock::given(path("/botbad:TOKEN/sendMessage"))
        .respond_with(ResponseTemplate::new(401).set_body_json(fixture("telegram_unauthorized.json")))
        .mount(&server)
        .await;
    Mock::given(path("/bot1:OK/sendMessage"))
        .respond_with(ResponseTemplate::new(400).set_body_json(fixture("telegram_chat_not_found.json")))
        .mount(&server)
        .await;
    let bad = notifier(
        "telegram",
        json!({ "bot_token": "bad:TOKEN", "chat_id": "1", "api_url": server.uri() }),
    );
    assert!(matches!(bad.test().await, Err(NotifyError::Unauthorized(_))));
    let missing = notifier(
        "telegram",
        json!({ "bot_token": "1:OK", "chat_id": "1", "api_url": server.uri() }),
    );
    match missing.test().await.unwrap_err() {
        NotifyError::Api(m) => assert!(m.contains("chat not found")),
        e => panic!("unexpected {e:?}"),
    }
}

#[tokio::test]
async fn pushover_form_and_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/1/messages.json"))
        .and(body_string_contains("token=apptoken"))
        .and(body_string_contains("user=goodkey"))
        .and(body_string_contains("priority=2"))
        .and(body_string_contains("retry=60"))
        .and(body_string_contains("html=1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pushover_ok.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("user=badkey"))
        .respond_with(ResponseTemplate::new(400).set_body_json(fixture("pushover_invalid_user.json")))
        .mount(&server)
        .await;
    let ok = notifier(
        "pushover",
        json!({ "api_token": "apptoken", "user_key": "goodkey", "priority": 2, "api_url": server.uri() }),
    );
    ok.notify(&grab()).await.unwrap();
    let bad = notifier(
        "pushover",
        json!({ "api_token": "apptoken", "user_key": "badkey", "api_url": server.uri() }),
    );
    match bad.test().await.unwrap_err() {
        NotifyError::Api(m) => assert!(m.contains("not a valid user")),
        e => panic!("unexpected {e:?}"),
    }
    let err = build(
        "pushover",
        &json!({ "api_token": "a", "user_key": "b", "priority": 7 }),
        &ctx(),
    )
    .err()
    .unwrap();
    assert!(matches!(err, NotifyError::InvalidSettings(_)));
}

#[tokio::test]
async fn gotify_uses_app_token_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gotify/message"))
        .and(header("X-Gotify-Key", "AbCdEf"))
        .and(body_partial_json(json!({
            "priority": 8,
            "extras": { "client::display": { "contentType": "text/markdown" } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("gotify_message.json")))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        "gotify",
        json!({ "server_url": format!("{}/gotify/", server.uri()), "app_token": "AbCdEf", "priority": 8 }),
    );
    n.notify(&grab()).await.unwrap();
}

#[tokio::test]
async fn ntfy_publishes_each_topic_with_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("Authorization", "Bearer tk_secret"))
        .and(body_partial_json(json!({ "markdown": true, "priority": 4 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("ntfy_message.json")))
        .expect(2)
        .mount(&server)
        .await;
    let n = notifier(
        "ntfy",
        json!({ "server_url": server.uri(), "topics": ["reelix", "family"],
                "access_token": "tk_secret", "priority": 4 }),
    );
    n.notify(&grab()).await.unwrap();
    let topics: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r: &Request| {
            r.body_json::<Value>().unwrap()["topic"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(topics, ["reelix", "family"]);
    assert!(build("ntfy", &json!({ "topics": ["a/b"] }), &ctx()).is_err());
}

#[tokio::test]
async fn apprise_stateful_and_stateless() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/notify/reelix"))
        .and(body_partial_json(
            json!({ "type": "info", "format": "markdown", "tag": "admins,tv" }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/notify"))
        .and(body_partial_json(json!({ "urls": "mailto://u:p@example.com" })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier(
        "apprise",
        json!({ "server_url": server.uri(), "config_key": "reelix", "tags": ["admins", "tv"] }),
    )
    .notify(&grab())
    .await
    .unwrap();
    notifier(
        "apprise",
        json!({ "server_url": server.uri(), "urls": "mailto://u:p@example.com" }),
    )
    .notify(&grab())
    .await
    .unwrap();
    assert!(build("apprise", &json!({ "server_url": server.uri() }), &ctx()).is_err());
}

#[tokio::test]
async fn matrix_resolves_alias_once_and_sends() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/_matrix/client/v3/directory/room/%23media%3Aexample.org"))
        .and(header("Authorization", "Bearer syt_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("matrix_alias.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(wiremock::matchers::path_regex(
            r"^/_matrix/client/v3/rooms/%21kXbAbCdEfGh%3Aexample.org/send/m.room.message/reelix-\d+-\d+$",
        ))
        .and(body_partial_json(json!({
            "msgtype": "m.notice",
            "format": "org.matrix.custom.html"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("matrix_event_id.json")))
        .expect(2)
        .mount(&server)
        .await;
    let n = notifier(
        "matrix",
        json!({ "homeserver_url": server.uri(), "access_token": "syt_token", "room": "#media:example.org" }),
    );
    n.notify(&grab()).await.unwrap();
    n.test().await.unwrap();
    // Transaction ids must differ between sends.
    let reqs = server.received_requests().await.unwrap();
    let puts: Vec<_> = reqs.iter().filter(|r| r.method.as_str() == "PUT").collect();
    assert_ne!(puts[0].url.path(), puts[1].url.path());
}

#[tokio::test]
async fn matrix_errors_are_typed() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(header("Authorization", "Bearer good"))
        .respond_with(ResponseTemplate::new(403).set_body_json(fixture("matrix_forbidden.json")))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(header("Authorization", "Bearer stale"))
        .respond_with(ResponseTemplate::new(401).set_body_json(fixture("matrix_unknown_token.json")))
        .mount(&server)
        .await;
    let room = "!kXbAbCdEfGh:example.org";
    let n = notifier(
        "matrix",
        json!({ "homeserver_url": server.uri(), "access_token": "good", "room": room }),
    );
    match n.test().await.unwrap_err() {
        NotifyError::Api(m) => assert!(m.starts_with("M_FORBIDDEN")),
        e => panic!("unexpected {e:?}"),
    }
    let n = notifier(
        "matrix",
        json!({ "homeserver_url": server.uri(), "access_token": "stale", "room": room }),
    );
    assert!(matches!(n.test().await, Err(NotifyError::Unauthorized(_))));
    assert!(
        build(
            "matrix",
            &json!({ "homeserver_url": server.uri(), "access_token": "x", "room": "media" }),
            &ctx()
        )
        .is_err()
    );
}

#[tokio::test]
async fn webhook_put_with_auth_headers_and_stable_payload() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/hook"))
        .and(header("X-Reelix-Source", "home"))
        .and(header_exists("Authorization"))
        .and(body_partial_json(json!({
            "version": 1,
            "event": "import",
            "event_kind": "upgrade",
            "instance": "Reelix",
            "data": { "is_upgrade": true, "media": { "kind": "series", "title": "Harbor Lights" } }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        "webhook",
        json!({ "url": format!("{}/hook", server.uri()), "method": "PUT",
                "username": "u", "password": "p", "headers": ["X-Reelix-Source: home"] }),
    );
    let upgrade = samples::all().remove(3);
    n.notify(&upgrade).await.unwrap();
    let req = &server.received_requests().await.unwrap()[0];
    assert_eq!(
        req.headers.get("authorization").unwrap().to_str().unwrap(),
        "Basic dTpw"
    );
    let body: Value = req.body_json().unwrap();
    for key in [
        "version",
        "event",
        "event_kind",
        "instance",
        "timestamp",
        "title",
        "message",
        "data",
    ] {
        assert!(body.get(key).is_some(), "missing {key}");
    }
    assert!(build("webhook", &json!({ "url": "ftp://x" }), &ctx()).is_err());
    assert!(
        build(
            "webhook",
            &json!({ "url": "http://x", "headers": ["nocolon"] }),
            &ctx()
        )
        .is_err()
    );
}

/// M7 exit criterion: every event type can be delivered to every (HTTP) notifier.
#[tokio::test]
async fn every_event_to_every_http_notifier() {
    let server = MockServer::start().await;
    Mock::given(path("/bot1:T/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("telegram_ok.json")))
        .mount(&server)
        .await;
    Mock::given(path("/1/messages.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pushover_ok.json")))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let uri = server.uri();
    let configs = [
        ("discord", json!({ "webhook_url": uri })),
        ("slack", json!({ "webhook_url": uri })),
        (
            "telegram",
            json!({ "bot_token": "1:T", "chat_id": "1", "api_url": uri }),
        ),
        (
            "pushover",
            json!({ "api_token": "a", "user_key": "b", "api_url": uri }),
        ),
        ("gotify", json!({ "server_url": uri, "app_token": "t" })),
        ("ntfy", json!({ "server_url": uri, "topics": ["t"] })),
        ("apprise", json!({ "server_url": uri, "config_key": "k" })),
        (
            "matrix",
            json!({ "homeserver_url": uri, "access_token": "t", "room": "!r:example.org" }),
        ),
        ("webhook", json!({ "url": uri })),
    ];
    let events = samples::all();
    for (imp, settings) in configs {
        let n = notifier(imp, settings);
        assert_eq!(n.implementation(), imp);
        for ev in &events {
            assert!(n.wants(ev));
            n.notify(ev)
                .await
                .unwrap_or_else(|e| panic!("{imp} / {:?}: {e}", ev.kind()));
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 9 * events.len());
}

#[test]
fn subscriptions_filter_events() {
    let n = notifier(
        "webhook",
        json!({ "url": "http://localhost/x", "events": ["grab", "request_pending"] }),
    );
    let events = samples::all();
    let wanted: Vec<_> = events.iter().filter(|e| n.wants(e)).map(|e| e.kind()).collect();
    use reelix_notify::EventKind::*;
    assert_eq!(wanted, [Test, Grab, RequestPending]);
}

#[test]
fn schemas_validate_required_fields() {
    for s in schemas() {
        if s.fields.iter().any(|f| f.required) {
            let err = build(&s.implementation, &json!({}), &ctx()).err();
            assert!(
                matches!(err, Some(NotifyError::InvalidSettings(_))),
                "{} accepted empty settings",
                s.implementation
            );
        }
    }
}
