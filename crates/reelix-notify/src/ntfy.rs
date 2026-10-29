//! ntfy publish (JSON body to the server root).

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError, Severity};

pub const ID: &str = "ntfy";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "ntfy",
        "Publishes to an ntfy topic (ntfy.sh or self-hosted)",
        Some("https://docs.ntfy.sh/publish/"),
        vec![
            FieldSchema::new("server_url", "Server URL", FieldKind::Url).default_value("https://ntfy.sh"),
            FieldSchema::new("topics", "Topics", FieldKind::List).required(),
            FieldSchema::new("access_token", "Access token", FieldKind::Secret)
                .help("Bearer token; alternatively use username/password"),
            FieldSchema::new("username", "Username", FieldKind::Text).advanced(),
            FieldSchema::new("password", "Password", FieldKind::Secret).advanced(),
            FieldSchema::new("priority", "Priority", FieldKind::Number)
                .default_value(3)
                .help("1 (min) – 5 (max)"),
            FieldSchema::new("tags", "Extra tags", FieldKind::List).advanced(),
            FieldSchema::new("click_url", "Click URL", FieldKind::Url)
                .help("Opened when the notification is tapped; defaults to the media page")
                .advanced(),
        ],
    )
}

/// ntfy notifier.
pub struct Ntfy {
    http: reqwest::Client,
    server_url: String,
    topics: Vec<String>,
    access_token: Option<String>,
    basic: Option<(String, String)>,
    priority: i64,
    tags: Vec<String>,
    click_url: Option<String>,
    events: EventMask,
}

impl Ntfy {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let topics = s.list("topics");
        if topics.is_empty() {
            return Err(NotifyError::settings("at least one topic is required"));
        }
        if let Some(bad) = topics.iter().find(|t| t.contains('/') || t.len() > 64) {
            return Err(NotifyError::settings(format!("invalid topic {bad:?}")));
        }
        Ok(Self {
            http: ctx.http.clone(),
            server_url: s.url("server_url")?,
            topics,
            access_token: s.opt_str("access_token"),
            basic: s
                .opt_str("username")
                .map(|u| (u, s.opt_str("password").unwrap_or_default())),
            priority: s.opt_i64("priority").unwrap_or(3).clamp(1, 5),
            tags: s.list("tags"),
            click_url: s.opt_str("click_url"),
            events: s.events()?,
        })
    }

    /// JSON body for one topic.
    pub fn payload(&self, topic: &str, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let emoji = match m.severity {
            Severity::Info => "information_source",
            Severity::Success => "white_check_mark",
            Severity::Warning => "warning",
            Severity::Failure => "rotating_light",
        };
        let mut tags = vec![emoji.to_string(), event.kind().as_str().to_string()];
        tags.extend(self.tags.iter().cloned());
        let mut body = json!({
            "topic": topic,
            "title": m.title,
            "message": m.markdown(),
            "markdown": true,
            "priority": self.priority,
            "tags": tags,
        });
        if let Some(c) = self.click_url.clone().or(m.url.clone()) {
            body["click"] = json!(c);
        }
        if let Some(img) = &m.image_url {
            body["icon"] = json!(img);
        }
        body
    }
}

#[async_trait]
impl Notifier for Ntfy {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        for topic in &self.topics {
            let mut req = self
                .http
                .post(format!("{}/", self.server_url))
                .json(&self.payload(topic, event));
            if let Some(t) = &self.access_token {
                req = req.bearer_auth(t);
            } else if let Some((u, p)) = &self.basic {
                req = req.basic_auth(u, Some(p));
            }
            check(req.send().await?).await?;
        }
        Ok(())
    }
}
