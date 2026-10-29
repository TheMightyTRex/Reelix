//! Matrix client-server API: send `m.room.message` to a room with an access token.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::common::{Settings, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "matrix";

pub fn schema() -> ProviderSchema {
    let opt = |v: &str, l: &str| SelectOption {
        value: json!(v),
        label: l.into(),
    };
    mk_schema(
        ID,
        "Matrix",
        "Sends messages to a Matrix room",
        Some("https://spec.matrix.org/latest/client-server-api/"),
        vec![
            FieldSchema::new("homeserver_url", "Homeserver URL", FieldKind::Url)
                .required()
                .placeholder("https://matrix.example.org"),
            FieldSchema::new("access_token", "Access token", FieldKind::Secret).required(),
            FieldSchema::new("room", "Room", FieldKind::Text)
                .required()
                .help("Room id (!abc:example.org) or alias (#media:example.org)"),
            FieldSchema::new(
                "msgtype",
                "Message type",
                FieldKind::Select {
                    options: vec![opt("m.notice", "Notice (bot style)"), opt("m.text", "Text")],
                },
            )
            .default_value("m.notice")
            .advanced(),
        ],
    )
}

#[derive(Deserialize)]
struct MatrixError {
    #[serde(default)]
    errcode: String,
    #[serde(default)]
    error: String,
}

#[derive(Deserialize)]
struct AliasResponse {
    room_id: String,
}

/// Matrix notifier. Room aliases are resolved once and cached.
pub struct Matrix {
    http: reqwest::Client,
    homeserver_url: String,
    access_token: String,
    room: String,
    msgtype: String,
    events: EventMask,
    room_id: OnceCell<String>,
    txn: AtomicU64,
}

/// Percent-encodes one path segment (room ids contain `!` and `:`).
fn seg(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

impl Matrix {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let room = s.str("room")?;
        if !(room.starts_with('!') || room.starts_with('#')) || !room.contains(':') {
            return Err(NotifyError::settings(
                "room must look like !id:server or #alias:server",
            ));
        }
        let msgtype = s.opt_str("msgtype").unwrap_or_else(|| "m.notice".into());
        if msgtype != "m.notice" && msgtype != "m.text" {
            return Err(NotifyError::settings("msgtype must be m.notice or m.text"));
        }
        Ok(Self {
            http: ctx.http.clone(),
            homeserver_url: s.url("homeserver_url")?,
            access_token: s.str("access_token")?,
            room,
            msgtype,
            events: s.events()?,
            room_id: OnceCell::new(),
            txn: AtomicU64::new(0),
        })
    }

    /// The `m.room.message` content for an event.
    pub fn content(&self, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let mut plain = format!("{}\n{}", m.title, m.text());
        if let Some(u) = &m.url {
            plain.push('\n');
            plain.push_str(u);
        }
        let mut html = m.html(true).replace('\n', "<br>");
        truncate(&mut html, 30_000);
        json!({
            "msgtype": self.msgtype,
            "body": plain,
            "format": "org.matrix.custom.html",
            "formatted_body": html,
        })
    }

    async fn error(resp: reqwest::Response) -> NotifyError {
        let status = resp.status().as_u16();
        if status == 429 {
            return NotifyError::RateLimited {
                retry_after_secs: None,
            };
        }
        let e: MatrixError = resp.json().await.unwrap_or(MatrixError {
            errcode: String::new(),
            error: String::new(),
        });
        match e.errcode.as_str() {
            "M_UNKNOWN_TOKEN" | "M_MISSING_TOKEN" => NotifyError::Unauthorized(e.error),
            "" => NotifyError::Status {
                status,
                body: e.error,
            },
            code => NotifyError::Api(format!("{code}: {}", e.error)),
        }
    }

    async fn resolve_room(&self) -> Result<&str, NotifyError> {
        self.room_id
            .get_or_try_init(|| async {
                if self.room.starts_with('!') {
                    return Ok(self.room.clone());
                }
                let url = format!(
                    "{}/_matrix/client/v3/directory/room/{}",
                    self.homeserver_url,
                    seg(&self.room)
                );
                let resp = self.http.get(url).bearer_auth(&self.access_token).send().await?;
                if !resp.status().is_success() {
                    return Err(Self::error(resp).await);
                }
                Ok(resp.json::<AliasResponse>().await?.room_id)
            })
            .await
            .map(String::as_str)
    }
}

#[async_trait]
impl Notifier for Matrix {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let room_id = self.resolve_room().await?;
        let txn = format!(
            "reelix-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            self.txn.fetch_add(1, Ordering::Relaxed)
        );
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver_url,
            seg(room_id),
            txn
        );
        let resp = self
            .http
            .put(url)
            .bearer_auth(&self.access_token)
            .json(&self.content(event))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Self::error(resp).await);
        }
        Ok(())
    }
}
