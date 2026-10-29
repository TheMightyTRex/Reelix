//! Plex PIN and OIDC logins against wiremock-served plex.tv / identity provider endpoints.

use std::sync::Arc;

use chrono::{Duration, Utc};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, EndUserEmail, EndUserUsername, IssuerUrl, JsonWebKeyId, Nonce,
    PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use reelix_api_types::requests::SecuritySettingsDto;
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_requests::fakes::{FakeDirectory, FakeLibrary, FakeMediaServer, FakeMetadata, FakeNotifications};
use reelix_requests::{Error, MediaServerKind, RequestDeps, RequestService};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = include_str!("fixtures/oidc_test_rsa.pem");

async fn service(dir: FakeDirectory) -> (RequestService, Db) {
    let db = Db::in_memory().await.unwrap();
    db.users()
        .create(NewUser {
            username: "admin".into(),
            email: None,
            display_name: None,
            password_hash: Some("x".into()),
            auth_source: AuthSource::Local,
            permissions: Permissions::ADMIN,
        })
        .await
        .unwrap();
    let svc = RequestService::new(RequestDeps {
        db: db.clone(),
        events: EventBus::default(),
        secrets: SecretBox::from_key([5; 32]),
        library: Arc::new(FakeLibrary::default()),
        media_servers: Arc::new(dir),
        metadata: Arc::new(FakeMetadata::default()),
        notifications: Arc::new(FakeNotifications::default()),
        http: None,
    });
    (svc, db)
}

#[tokio::test]
async fn plex_pin_login_requires_server_membership() {
    let plex = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/pins"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 77, "code": "abcd"})))
        .mount(&plex)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/pins/77"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 77, "code": "abcd", "authToken": "plex-tok"})),
        )
        .mount(&plex)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/pins/78"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": 78, "code": "efgh", "authToken": null})),
        )
        .mount(&plex)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": 42, "username": "plexfan", "email": "fan@plex.tv"})),
        )
        .mount(&plex)
        .await;

    let member = FakeMediaServer {
        kind: MediaServerKind::Plex,
        users: vec![(FakeMediaServer::user("42", "plexfan"), None)],
    };
    let (svc, db) = service(FakeDirectory::new(&plex.uri()).with(1, "Plex", member)).await;
    assert!(matches!(
        svc.plex_create_pin(None).await,
        Err(Error::Forbidden(_))
    ));
    let s = SecuritySettingsDto {
        plex_login: true,
        ..Default::default()
    };
    svc.set_security_settings(s).await.unwrap();

    let pin = svc.plex_create_pin(Some("http://localhost/close")).await.unwrap();
    assert_eq!(pin.id, 77);
    assert!(pin.auth_url.contains("code=abcd"));
    assert!(
        svc.plex_check_pin(78).await.unwrap().is_none(),
        "not approved yet"
    );
    let user = svc.plex_check_pin(77).await.unwrap().unwrap();
    assert_eq!(user.username, "plexfan");
    assert_eq!(user.auth_source, AuthSource::Plex);
    let token: Option<String> = sqlx::query_scalar("SELECT plex_token FROM users WHERE id = ?")
        .bind(user.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let token = token.unwrap();
    assert_ne!(token, "plex-tok", "token is stored encrypted");
    // Same account logs in again → same user.
    assert_eq!(svc.plex_check_pin(77).await.unwrap().unwrap().id, user.id);

    // Not a member of any configured server → rejected.
    let stranger = FakeMediaServer {
        kind: MediaServerKind::Plex,
        users: vec![(FakeMediaServer::user("1", "owner"), None)],
    };
    let (svc, _) = service(FakeDirectory::new(&plex.uri()).with(1, "Plex", stranger)).await;
    let s = SecuritySettingsDto {
        plex_login: true,
        ..Default::default()
    };
    svc.set_security_settings(s).await.unwrap();
    assert!(matches!(svc.plex_check_pin(77).await, Err(Error::Forbidden(_))));
}

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("{name} missing in {url}"))
}

fn id_token(issuer: &str, nonce: &str, key: &CoreRsaPrivateSigningKey) -> String {
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer.into()).unwrap(),
        vec![Audience::new("reelix".into())],
        Utc::now() + Duration::minutes(5),
        Utc::now(),
        StandardClaims::new(SubjectIdentifier::new("sub-123".into()))
            .set_preferred_username(Some(EndUserUsername::new("sso-user".into())))
            .set_email(Some(EndUserEmail::new("sso@example.com".into()))),
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce.into())));
    let token = CoreIdToken::new(
        claims,
        key,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .unwrap();
    serde_json::to_value(&token).unwrap().as_str().unwrap().to_owned()
}

#[tokio::test]
async fn oidc_code_flow_with_pkce() {
    let idp = MockServer::start().await;
    let base = idp.uri();
    let key = CoreRsaPrivateSigningKey::from_pem(KEY, Some(JsonWebKeyId::new("k1".into()))).unwrap();
    let jwks = CoreJsonWebKeySet::new(vec![key.as_verification_key()]);
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "jwks_uri": format!("{base}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        })))
        .mount(&idp)
        .await;
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::to_value(&jwks).unwrap()))
        .mount(&idp)
        .await;

    let (svc, _) = service(FakeDirectory::new("http://127.0.0.1:9")).await;
    let cb = "http://reelix.local/api/v1/auth/oidc/callback";
    assert!(matches!(svc.oidc_begin(cb, None).await, Err(Error::Forbidden(_))));
    let mut s = SecuritySettingsDto::default();
    s.oidc.enabled = true;
    s.oidc.issuer_url = base.clone();
    s.oidc.client_id = "reelix".into();
    s.oidc.client_secret = "secret".into();
    svc.set_security_settings(s.clone()).await.unwrap();

    let url = svc.oidc_begin(cb, Some("/requests")).await.unwrap();
    assert!(url.starts_with(&format!("{base}/authorize?")));
    assert_eq!(query_param(&url, "code_challenge_method"), "S256");
    assert_eq!(query_param(&url, "redirect_uri"), cb);
    assert!(query_param(&url, "scope").contains("email"));
    let state = query_param(&url, "state");
    let nonce = query_param(&url, "nonce");

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(wiremock::matchers::body_string_contains("code_verifier="))
        .and(wiremock::matchers::body_string_contains("code=the-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at",
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token(&base, &nonce, &key),
        })))
        .mount(&idp)
        .await;

    // Wrong state is rejected.
    assert!(matches!(
        svc.oidc_complete("the-code", "bogus").await,
        Err(Error::Unauthorized(_))
    ));
    let (user, to) = svc.oidc_complete("the-code", &state).await.unwrap();
    assert_eq!(to, "/requests");
    assert_eq!(user.username, "sso-user");
    assert_eq!(user.email.as_deref(), Some("sso@example.com"));
    assert_eq!(user.auth_source, AuthSource::Oidc);
    // State is single use.
    assert!(matches!(
        svc.oidc_complete("the-code", &state).await,
        Err(Error::Unauthorized(_))
    ));

    // A token with the wrong nonce fails verification.
    let url = svc.oidc_begin(cb, None).await.unwrap();
    let state2 = query_param(&url, "state");
    idp.reset().await;
    for (p, body) in [
        (
            "/.well-known/openid-configuration",
            json!({
                "issuer": base, "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"), "jwks_uri": format!("{base}/jwks"),
                "response_types_supported": ["code"], "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
            }),
        ),
        ("/jwks", serde_json::to_value(&jwks).unwrap()),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&idp)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at", "token_type": "Bearer",
            "id_token": id_token(&base, "other-nonce", &key),
        })))
        .mount(&idp)
        .await;
    assert!(matches!(
        svc.oidc_complete("c", &state2).await,
        Err(Error::Unauthorized(_))
    ));

    // auto_create off → unknown subject rejected, known subject still logs in.
    s.oidc.client_secret = "********".into();
    s.oidc.auto_create = false;
    svc.set_security_settings(s).await.unwrap();
    let url = svc.oidc_begin(cb, None).await.unwrap();
    let st = query_param(&url, "state");
    let nonce = query_param(&url, "nonce");
    idp.reset().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": base, "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"), "jwks_uri": format!("{base}/jwks"),
            "response_types_supported": ["code"], "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        })))
        .mount(&idp)
        .await;
    Mock::given(method("GET"))
        .and(path("/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::to_value(&jwks).unwrap()))
        .mount(&idp)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at", "token_type": "Bearer", "id_token": id_token(&base, &nonce, &key),
        })))
        .mount(&idp)
        .await;
    let (again, _) = svc.oidc_complete("c", &st).await.unwrap();
    assert_eq!(again.id, user.id);
}
