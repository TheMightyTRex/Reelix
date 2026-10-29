//! Apprise API (`/notify/{key}` stateful or `/notify` stateless).

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "apprise";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Apprise",
        "Fans out through an Apprise API server to 100+ services",
        Some("https://github.com/caronc/apprise-api"),
        vec![
            FieldSchema::new("server_url", "Apprise API URL", FieldKind::Url)
                .required()
                .placeholder("http://apprise:8000"),
            FieldSchema::new("config_key", "Configuration key", FieldKind::Text)
                .help("Stateful key configured on the Apprise server"),
            FieldSchema::new("urls", "Apprise URLs", FieldKind::Secret)
                .help("Stateless mode: comma-separated Apprise URLs (used when no key is set)"),
            FieldSchema::new("tags", "Tags", FieldKind::List)
                .help("Only notify URLs with these tags (stateful mode)"),
            FieldSchema::new("username", "Username", FieldKind::Text).advanced(),
            FieldSchema::new("password", "Password", FieldKind::Secret).advanced(),
        ],
    )
}

/// Apprise API notifier.
pub struct Apprise {
    http: reqwest::Client,
    server_url: String,
    config_key: Option<String>,
    urls: Option<String>,
    tags: Vec<String>,
    basic: Option<(String, String)>,
    events: EventMask,
}

impl Apprise {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let config_key = s.opt_str("config_key");
        let urls = s.opt_str("urls");
        if config_key.is_none() && urls.is_none() {
            return Err(NotifyError::settings(
                "either a configuration key or Apprise URLs are required",
            ));
        }
        if let Some(k) = &config_key
            && !k
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(NotifyError::settings(
                "configuration key may only contain [A-Za-z0-9_-]",
            ));
        }
        Ok(Self {
            http: ctx.http.clone(),
            server_url: s.url("server_url")?,
            config_key,
            urls,
            tags: s.list("tags"),
            basic: s
                .opt_str("username")
                .map(|u| (u, s.opt_str("password").unwrap_or_default())),
            events: s.events()?,
        })
    }

    /// Endpoint URL and JSON body.
    pub fn request(&self, event: &NotificationEvent) -> (String, Value) {
        let m = Message::from_event(event);
        let mut body = json!({
            "title": m.title,
            "body": m.markdown(),
            "type": m.severity.as_str(),
            "format": "markdown",
        });
        let url = match &self.config_key {
            Some(k) => {
                if !self.tags.is_empty() {
                    body["tag"] = json!(self.tags.join(","));
                }
                format!("{}/notify/{k}", self.server_url)
            }
            None => {
                body["urls"] = json!(self.urls.clone().unwrap_or_default());
                format!("{}/notify", self.server_url)
            }
        };
        (url, body)
    }
}

#[async_trait]
impl Notifier for Apprise {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let (url, body) = self.request(event);
        let mut req = self.http.post(url).json(&body);
        if let Some((u, p)) = &self.basic {
            req = req.basic_auth(u, Some(p));
        }
        check(req.send().await?).await?;
        Ok(())
    }
}
