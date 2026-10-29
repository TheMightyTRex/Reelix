//! Discord incoming webhook with a rich embed.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "discord";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Discord",
        "Posts an embed to a Discord channel webhook",
        Some("https://support.discord.com/hc/en-us/articles/228383668"),
        vec![
            FieldSchema::new("webhook_url", "Webhook URL", FieldKind::Secret)
                .required()
                .placeholder("https://discord.com/api/webhooks/…"),
            FieldSchema::new("username", "Username", FieldKind::Text)
                .help("Overrides the webhook's default name"),
            FieldSchema::new("avatar_url", "Avatar URL", FieldKind::Url).advanced(),
            FieldSchema::new("include_poster", "Include poster", FieldKind::Bool).default_value(true),
        ],
    )
}

/// Discord webhook notifier.
pub struct Discord {
    http: reqwest::Client,
    webhook_url: String,
    username: String,
    avatar_url: Option<String>,
    include_poster: bool,
    events: EventMask,
}

impl Discord {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            webhook_url: s.url("webhook_url")?,
            username: s.opt_str("username").unwrap_or_else(|| ctx.app_name.clone()),
            avatar_url: s.opt_str("avatar_url"),
            include_poster: s.bool("include_poster"),
            events: s.events()?,
        })
    }

    /// The JSON body posted to the webhook.
    pub fn payload(&self, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let fields: Vec<Value> = m
            .fields
            .iter()
            .take(25)
            .map(|f| {
                let mut name = f.name.clone();
                truncate(&mut name, 256);
                let mut value = f.value.clone();
                truncate(&mut value, 1024);
                json!({ "name": name, "value": value, "inline": value.len() < 40 })
            })
            .collect();
        let mut title = m.title.clone();
        truncate(&mut title, 256);
        let mut description = m.body.clone();
        truncate(&mut description, 4096);
        let mut embed = json!({
            "title": title,
            "description": description,
            "color": m.severity.color(),
            "fields": fields,
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "footer": { "text": self.username },
        });
        if let Some(u) = &m.url {
            embed["url"] = json!(u);
        }
        if self.include_poster
            && let Some(img) = &m.image_url
        {
            embed["thumbnail"] = json!({ "url": img });
        }
        let mut body = json!({ "username": self.username, "embeds": [embed] });
        if let Some(a) = &self.avatar_url {
            body["avatar_url"] = json!(a);
        }
        body
    }
}

#[async_trait]
impl Notifier for Discord {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let resp = self
            .http
            .post(&self.webhook_url)
            .query(&[("wait", "true")])
            .json(&self.payload(event))
            .send()
            .await?;
        check(resp).await?;
        Ok(())
    }
}
