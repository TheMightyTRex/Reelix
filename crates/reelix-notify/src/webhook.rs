//! Generic JSON webhook.
//!
//! # Payload (version 1, stable)
//!
//! ```json
//! {
//!   "version": 1,
//!   "event": "import",              // NotificationEvent variant (snake_case)
//!   "event_kind": "upgrade",        // EventKind (imports that replaced a file are "upgrade")
//!   "instance": "Reelix",
//!   "timestamp": "2026-01-01T12:00:00Z",
//!   "title": "Upgraded: Harbor Lights (2021) - S02E05",
//!   "message": "plain text body with `Name: value` lines",
//!   "data": { ... }                 // the event payload, see `event` module types
//! }
//! ```
//!
//! Fields may be added in later versions; existing fields are never renamed or removed.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "webhook";

/// Current payload version.
pub const PAYLOAD_VERSION: u32 = 1;

pub fn schema() -> ProviderSchema {
    let opt = |v: &str| SelectOption {
        value: json!(v),
        label: v.into(),
    };
    mk_schema(
        ID,
        "Webhook",
        "POSTs or PUTs a documented JSON payload to any URL",
        None,
        vec![
            FieldSchema::new("url", "URL", FieldKind::Url).required(),
            FieldSchema::new(
                "method",
                "Method",
                FieldKind::Select {
                    options: vec![opt("POST"), opt("PUT")],
                },
            )
            .default_value("POST"),
            FieldSchema::new("username", "Username", FieldKind::Text),
            FieldSchema::new("password", "Password", FieldKind::Secret),
            FieldSchema::new("headers", "Extra headers", FieldKind::List)
                .help("One `Name: value` per entry")
                .advanced(),
        ],
    )
}

/// The documented webhook body (also exposed to custom scripts as `REELIX_EVENT_JSON`).
pub fn payload(event: &NotificationEvent, instance: &str) -> Value {
    let m = Message::from_event(event);
    let mut ev = serde_json::to_value(event).unwrap_or(Value::Null);
    let data = ev.get_mut("data").map(Value::take).unwrap_or_else(|| json!({}));
    json!({
        "version": PAYLOAD_VERSION,
        "event": ev.get("event").cloned().unwrap_or(Value::Null),
        "event_kind": event.kind().as_str(),
        "instance": instance,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "title": m.title,
        "message": m.text(),
        "data": data,
    })
}

/// Webhook notifier.
pub struct Webhook {
    http: reqwest::Client,
    url: String,
    put: bool,
    basic: Option<(String, String)>,
    headers: HeaderMap,
    instance: String,
    events: EventMask,
}

impl Webhook {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let put = match s.opt_str("method").as_deref() {
            None | Some("POST") | Some("post") => false,
            Some("PUT") | Some("put") => true,
            Some(o) => return Err(NotifyError::settings(format!("unsupported method {o}"))),
        };
        let mut headers = HeaderMap::new();
        for h in s.list("headers") {
            let (name, value) = h
                .split_once(':')
                .ok_or_else(|| NotifyError::settings(format!("header {h:?} must be `Name: value`")))?;
            let name = HeaderName::try_from(name.trim())
                .map_err(|_| NotifyError::settings(format!("invalid header name {name:?}")))?;
            let value = HeaderValue::try_from(value.trim())
                .map_err(|_| NotifyError::settings(format!("invalid value for header {name}")))?;
            headers.append(name, value);
        }
        Ok(Self {
            http: ctx.http.clone(),
            url: s.url("url")?,
            put,
            basic: s
                .opt_str("username")
                .map(|u| (u, s.opt_str("password").unwrap_or_default())),
            headers,
            instance: ctx.app_name.clone(),
            events: s.events()?,
        })
    }
}

#[async_trait]
impl Notifier for Webhook {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let req = if self.put {
            self.http.put(&self.url)
        } else {
            self.http.post(&self.url)
        };
        let mut req = req
            .headers(self.headers.clone())
            .json(&payload(event, &self.instance));
        if let Some((u, p)) = &self.basic {
            req = req.basic_auth(u, Some(p));
        }
        check(req.send().await?).await?;
        Ok(())
    }
}
