//! Gotify `/message` API with markdown extras.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "gotify";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Gotify",
        "Pushes messages to a self-hosted Gotify server",
        Some("https://gotify.net/docs/pushmsg"),
        vec![
            FieldSchema::new("server_url", "Server URL", FieldKind::Url)
                .required()
                .placeholder("https://gotify.example.com"),
            FieldSchema::new("app_token", "Application token", FieldKind::Secret).required(),
            FieldSchema::new("priority", "Priority", FieldKind::Number)
                .default_value(5)
                .help("0–10; 8+ is shown as high priority on Android"),
            FieldSchema::new("include_poster", "Include poster", FieldKind::Bool).default_value(false),
        ],
    )
}

/// Gotify notifier.
pub struct Gotify {
    http: reqwest::Client,
    server_url: String,
    app_token: String,
    priority: i64,
    include_poster: bool,
    events: EventMask,
}

impl Gotify {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            server_url: s.url("server_url")?,
            app_token: s.str("app_token")?,
            priority: s.opt_i64("priority").unwrap_or(5).clamp(0, 10),
            include_poster: s.bool("include_poster"),
            events: s.events()?,
        })
    }

    /// JSON body for `POST /message`.
    pub fn payload(&self, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let mut message = m.markdown();
        if self.include_poster
            && let Some(img) = &m.image_url
        {
            message = format!("![poster]({img})\n\n{message}");
        }
        let mut extras = json!({ "client::display": { "contentType": "text/markdown" } });
        if let Some(u) = &m.url {
            extras["client::notification"] = json!({ "click": { "url": u } });
        }
        json!({
            "title": m.title,
            "message": message,
            "priority": self.priority,
            "extras": extras,
        })
    }
}

#[async_trait]
impl Notifier for Gotify {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let resp = self
            .http
            .post(format!("{}/message", self.server_url))
            .header("X-Gotify-Key", &self.app_token)
            .json(&self.payload(event))
            .send()
            .await?;
        check(resp).await?;
        Ok(())
    }
}
