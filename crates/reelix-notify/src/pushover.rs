//! Pushover messages API.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "pushover";
const DEFAULT_API: &str = "https://api.pushover.net";

pub fn schema() -> ProviderSchema {
    let p = |v: i64, l: &str| SelectOption {
        value: json!(v),
        label: l.into(),
    };
    mk_schema(
        ID,
        "Pushover",
        "Push notifications through Pushover",
        Some("https://pushover.net/api"),
        vec![
            FieldSchema::new("api_token", "Application token", FieldKind::Secret).required(),
            FieldSchema::new("user_key", "User or group key", FieldKind::Secret).required(),
            FieldSchema::new("devices", "Devices", FieldKind::List)
                .help("Leave empty to send to all devices"),
            FieldSchema::new(
                "priority",
                "Priority",
                FieldKind::Select {
                    options: vec![
                        p(-2, "Lowest"),
                        p(-1, "Low"),
                        p(0, "Normal"),
                        p(1, "High"),
                        p(2, "Emergency"),
                    ],
                },
            )
            .default_value(0),
            FieldSchema::new("retry", "Retry (seconds)", FieldKind::Number)
                .default_value(60)
                .help("Emergency priority only; minimum 30")
                .advanced(),
            FieldSchema::new("expire", "Expire (seconds)", FieldKind::Number)
                .default_value(3600)
                .help("Emergency priority only; maximum 10800")
                .advanced(),
            FieldSchema::new("sound", "Sound", FieldKind::Text).advanced(),
            FieldSchema::new("api_url", "API URL", FieldKind::Url)
                .default_value(DEFAULT_API)
                .advanced(),
        ],
    )
}

#[derive(Deserialize)]
struct PoResponse {
    status: i64,
    #[serde(default)]
    errors: Vec<String>,
}

/// Pushover notifier.
pub struct Pushover {
    http: reqwest::Client,
    api_url: String,
    api_token: String,
    user_key: String,
    devices: Vec<String>,
    priority: i64,
    retry: i64,
    expire: i64,
    sound: Option<String>,
    events: EventMask,
}

impl Pushover {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let priority = s.opt_i64("priority").unwrap_or(0);
        if !(-2..=2).contains(&priority) {
            return Err(NotifyError::settings("priority must be between -2 and 2"));
        }
        Ok(Self {
            http: ctx.http.clone(),
            api_url: s.url("api_url")?,
            api_token: s.str("api_token")?,
            user_key: s.str("user_key")?,
            devices: s.list("devices"),
            priority,
            retry: s.opt_i64("retry").unwrap_or(60).max(30),
            expire: s.opt_i64("expire").unwrap_or(3600).clamp(30, 10800),
            sound: s.opt_str("sound"),
            events: s.events()?,
        })
    }

    /// Form fields posted to `/1/messages.json`.
    pub fn form(&self, event: &NotificationEvent) -> Vec<(&'static str, String)> {
        let m = Message::from_event(event);
        let mut title = m.title.clone();
        truncate(&mut title, 250);
        let mut msg = m.html(false);
        truncate(&mut msg, 1024);
        let mut f = vec![
            ("token", self.api_token.clone()),
            ("user", self.user_key.clone()),
            ("title", title),
            ("message", msg),
            ("html", "1".into()),
            ("priority", self.priority.to_string()),
        ];
        if self.priority == 2 {
            f.push(("retry", self.retry.to_string()));
            f.push(("expire", self.expire.to_string()));
        }
        if !self.devices.is_empty() {
            f.push(("device", self.devices.join(",")));
        }
        if let Some(s) = &self.sound {
            f.push(("sound", s.clone()));
        }
        if let Some(u) = &m.url {
            f.push(("url", u.clone()));
            f.push(("url_title", "More info".into()));
        }
        f
    }
}

#[async_trait]
impl Notifier for Pushover {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let url = format!("{}/1/messages.json", self.api_url);
        let resp = self.http.post(url).form(&self.form(event)).send().await?;
        // Pushover returns 4xx with `{status:0, errors:[…]}` for bad input/keys.
        if resp.status().as_u16() == 400 {
            let r: PoResponse = resp.json().await?;
            return Err(NotifyError::Api(r.errors.join("; ")));
        }
        let r: PoResponse = check(resp).await?.json().await?;
        if r.status != 1 {
            return Err(NotifyError::Api(r.errors.join("; ")));
        }
        Ok(())
    }
}
