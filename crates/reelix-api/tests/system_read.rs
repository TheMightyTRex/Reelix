//! VIEW_SYSTEM (RFX-84): a manager key reads the logs, jobs, scheduled tasks and backup list
//! without the admin right; every system change and the backup download stay admin only.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use reelix_api::{
    ApiConfig, ApiError, ApiResult, AppState, LogsQuery, Maintenance, MaintenanceBackend, UploadStream,
};
use reelix_api_types::system::{
    BackupDto, BackupKind, BackupSettings, BackupsOverview, LogEntryDto, LogFileDto, LogLevel, LogsDto,
    PendingRestoreDto, ServerSettingsDto, ServerSettingsValues, UpdateSettings, UpdateStatusDto,
};
use reelix_core::bus::EventBus;
use reelix_core::{AuthSource, Permissions};
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewUser};
use reelix_jobs::JobSystem;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Answers the read calls with fixed data; every change fails the test if it is reached.
struct FakeMaintenance {
    log_file: PathBuf,
}

fn not_for_readers<T>() -> BoxFuture<'static, ApiResult<T>> {
    panic!("a change reached the backend without the admin right")
}

impl MaintenanceBackend for FakeMaintenance {
    fn backups(&self) -> BoxFuture<'_, ApiResult<BackupsOverview>> {
        Box::pin(async {
            Ok(BackupsOverview {
                backups: vec![BackupDto {
                    name: "reelix_backup_2026-11-13.zip".into(),
                    kind: BackupKind::Scheduled,
                    size_bytes: 1024,
                    created_at: chrono::Utc::now(),
                    version: Some("0.8.7".into()),
                }],
                settings: BackupSettings::default(),
                backup_dir: "/config/backups".into(),
                pending_restore: None,
                includes_secret_key: true,
            })
        })
    }
    fn create_backup(&self, _: BackupKind) -> BoxFuture<'_, ApiResult<BackupDto>> {
        not_for_readers()
    }
    fn backup_path(&self, _: &str) -> ApiResult<PathBuf> {
        panic!("a backup download reached the backend without the admin right")
    }
    fn delete_backup(&self, _: String) -> BoxFuture<'_, ApiResult<()>> {
        not_for_readers()
    }
    fn set_backup_settings(&self, _: BackupSettings) -> BoxFuture<'_, ApiResult<BackupSettings>> {
        not_for_readers()
    }
    fn stage_restore(&self, _: String) -> BoxFuture<'_, ApiResult<PendingRestoreDto>> {
        not_for_readers()
    }
    fn stage_restore_upload(&self, _: UploadStream) -> BoxFuture<'_, ApiResult<PendingRestoreDto>> {
        not_for_readers()
    }
    fn cancel_restore(&self) -> BoxFuture<'_, ApiResult<()>> {
        not_for_readers()
    }
    fn log_files(&self) -> BoxFuture<'_, ApiResult<Vec<LogFileDto>>> {
        Box::pin(async {
            Ok(vec![LogFileDto {
                name: "reelix.log".into(),
                size_bytes: 12,
                modified: chrono::Utc::now(),
            }])
        })
    }
    fn log_path(&self, name: &str) -> ApiResult<PathBuf> {
        if name == "reelix.log" {
            Ok(self.log_file.clone())
        } else {
            Err(ApiError::not_found("unknown log file"))
        }
    }
    fn logs(&self, _: LogsQuery) -> BoxFuture<'_, ApiResult<LogsDto>> {
        Box::pin(async {
            Ok(LogsDto {
                file: Some("reelix.log".into()),
                entries: vec![LogEntryDto {
                    time: None,
                    level: Some(LogLevel::Warn),
                    target: Some("reelix".into()),
                    message: "Indexer Example is failing".into(),
                }],
                truncated: false,
            })
        })
    }
    fn update_status(&self, _: bool) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        not_for_readers()
    }
    fn set_update_settings(&self, _: UpdateSettings) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        not_for_readers()
    }
    fn install_update(&self, _: String) -> BoxFuture<'_, ApiResult<UpdateStatusDto>> {
        not_for_readers()
    }
    fn server_settings(&self) -> BoxFuture<'_, ApiResult<ServerSettingsDto>> {
        not_for_readers()
    }
    fn set_server_settings(&self, _: ServerSettingsValues) -> BoxFuture<'_, ApiResult<ServerSettingsDto>> {
        not_for_readers()
    }
}

struct TestApp {
    router: Router,
    manager: String,
    admin: String,
    user: String,
    _data: tempfile::TempDir,
}

async fn key_for(db: &Db, name: &str, permissions: Permissions) -> String {
    let user = db
        .users()
        .create(NewUser {
            username: name.into(),
            email: None,
            display_name: None,
            password_hash: None,
            auth_source: AuthSource::Local,
            permissions,
        })
        .await
        .unwrap();
    let key = reelix_api::auth::random_token();
    db.api_keys()
        .create(user.id, "test", &key[..8], &reelix_api::auth::sha256_hex(&key))
        .await
        .unwrap();
    key
}

async fn app() -> TestApp {
    let data = tempfile::tempdir().unwrap();
    let log_file = data.path().join("reelix.log");
    std::fs::write(&log_file, "a log line\n").unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let system = JobSystem::builder(db.clone(), events.clone()).build();
    let config = ApiConfig {
        url_base: String::new(),
        data_dir: data.path().to_path_buf(),
        database: "sqlite".into(),
        start_time: chrono::Utc::now(),
    };
    let manager = key_for(&db, "manager", Permissions::ROLE_MANAGER).await;
    let admin = key_for(&db, "admin", Permissions::ADMIN).await;
    let user = key_for(&db, "user", Permissions::ROLE_POWER_USER).await;
    let state = AppState::new(db, events, system.queue(), SecretBox::from_key([5u8; 32]), config)
        .with_extension(Arc::new(Maintenance(Arc::new(FakeMaintenance { log_file }))));
    TestApp {
        router: reelix_api::router(state),
        manager,
        admin,
        user,
        _data: data,
    }
}

async fn call(
    app: &TestApp,
    key: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", key);
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

const READS: [&str; 6] = [
    "/api/v1/system/logs",
    "/api/v1/system/logs/files",
    "/api/v1/system/logs/files/reelix.log",
    "/api/v1/system/jobs",
    "/api/v1/system/tasks",
    "/api/v1/system/backups",
];

#[tokio::test]
async fn manager_key_reads_logs_jobs_tasks_and_backups() {
    let app = app().await;
    for uri in READS {
        let (s, v) = call(&app, &app.manager, Method::GET, uri, None).await;
        assert_eq!(s, StatusCode::OK, "{uri}: {v}");
    }
    let (_, logs) = call(&app, &app.manager, Method::GET, "/api/v1/system/logs", None).await;
    assert_eq!(logs["entries"][0]["message"], "Indexer Example is failing");
    let (_, backups) = call(&app, &app.manager, Method::GET, "/api/v1/system/backups", None).await;
    assert_eq!(backups["backups"][0]["name"], "reelix_backup_2026-11-13.zip");
    assert_eq!(backups["backup_dir"], "", "the server path is only for admins");
    let (_, backups) = call(&app, &app.admin, Method::GET, "/api/v1/system/backups", None).await;
    assert_eq!(backups["backup_dir"], "/config/backups");
}

#[tokio::test]
async fn reads_need_view_system() {
    let app = app().await;
    for uri in READS {
        let (s, v) = call(&app, &app.user, Method::GET, uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}: {v}");
    }
}

#[tokio::test]
async fn changes_and_backup_downloads_stay_admin_only() {
    let app = app().await;
    let changes: Vec<(Method, &str, Option<Value>)> = vec![
        (
            Method::GET,
            "/api/v1/system/backups/reelix_backup_2026-11-13.zip",
            None,
        ),
        (Method::POST, "/api/v1/system/backups", None),
        (
            Method::PUT,
            "/api/v1/system/backups/settings",
            Some(json!({"interval_days": 1, "retention": 2})),
        ),
        (
            Method::DELETE,
            "/api/v1/system/backups/reelix_backup_2026-11-13.zip",
            None,
        ),
        (
            Method::POST,
            "/api/v1/system/backups/reelix_backup_2026-11-13.zip/restore",
            None,
        ),
        (Method::DELETE, "/api/v1/system/backups/restore", None),
        (Method::POST, "/api/v1/system/tasks/housekeeping/run", None),
        (Method::GET, "/api/v1/system/background-work", None),
        (
            Method::PUT,
            "/api/v1/system/background-work",
            Some(json!({"pace": "normal"})),
        ),
        (Method::GET, "/api/v1/system/updates", None),
        (Method::POST, "/api/v1/system/updates/check", None),
        (Method::GET, "/api/v1/system/settings", None),
    ];
    for (method, uri, body) in changes {
        let (s, v) = call(&app, &app.manager, method.clone(), uri, body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}: {v}");
    }
}

#[tokio::test]
async fn view_system_is_listed_with_the_other_rights() {
    let app = app().await;
    let (s, v) = call(&app, &app.admin, Method::GET, "/api/v1/users/roles", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let right = v["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "VIEW_SYSTEM")
        .expect("VIEW_SYSTEM is listed");
    assert_eq!(right["bit"], 1u64 << 22);
    let manager = v["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "manager")
        .unwrap();
    assert_ne!(manager["permissions"].as_u64().unwrap() & (1 << 22), 0);
}
