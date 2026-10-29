//! Slack incoming webhook using Block Kit.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde_json::{Value, json};

use crate::common::{Settings, check, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "slack";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Slack",
        "Posts Block Kit messages to a Slack incoming webhook",
        Some("https://api.slack.com/messaging/webhooks"),
        vec![
            FieldSchema::new("webhook_url", "Webhook URL", FieldKind::Secret)
                .required()
                .placeholder("https://hooks.slack.com/services/…"),
            FieldSchema::new("channel", "Channel", FieldKind::Text)
                .help("Overrides the webhook's channel (legacy webhooks only)")
                .advanced(),
            FieldSchema::new("username", "Username", FieldKind::Text).advanced(),
            FieldSchema::new("icon", "Icon", FieldKind::Text)
                .help("Emoji like :movie_camera: or an image URL")
                .advanced(),
        ],
    )
}

/// Escapes `& < >` per Slack mrkdwn rules.
pub fn escape_mrkdwn(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Slack webhook notifier.
pub struct Slack {
    http: reqwest::Client,
    webhook_url: String,
    channel: Option<String>,
    username: Option<String>,
    icon: Option<String>,
    events: EventMask,
}

impl Slack {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        Ok(Self {
            http: ctx.http.clone(),
            webhook_url: s.url("webhook_url")?,
            channel: s.opt_str("channel"),
            username: s.opt_str("username"),
            icon: s.opt_str("icon"),
            events: s.events()?,
        })
    }

    /// The JSON body posted to the webhook.
    pub fn payload(&self, event: &NotificationEvent) -> Value {
        let m = Message::from_event(event);
        let mut header = m.title.clone();
        truncate(&mut header, 150);
        let mut blocks = vec![json!({
            "type": "header",
            "text": { "type": "plain_text", "text": header, "emoji": true }
        })];
        if !m.body.is_empty() {
            let mut text = escape_mrkdwn(&m.body);
            truncate(&mut text, 3000);
            let mut section = json!({ "type": "section", "text": { "type": "mrkdwn", "text": text } });
            if let Some(img) = &m.image_url {
                section["accessory"] = json!({ "type": "image", "image_url": img, "alt_text": "poster" });
            }
            blocks.push(section);
        }
        if !m.fields.is_empty() {
            let fields: Vec<Value> = m
                .fields
                .iter()
                .take(10)
                .map(|f| {
                    let mut t = format!("*{}*\n{}", escape_mrkdwn(&f.name), escape_mrkdwn(&f.value));
                    truncate(&mut t, 2000);
                    json!({ "type": "mrkdwn", "text": t })
                })
                .collect();
            blocks.push(json!({ "type": "section", "fields": fields }));
        }
        if let Some(u) = &m.url {
            blocks.push(json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": format!("<{}|More info>", u) }]
            }));
        }
        let mut body = json!({ "text": m.title, "blocks": blocks });
        if let Some(c) = &self.channel {
            body["channel"] = json!(c);
        }
        if let Some(u) = &self.username {
            body["username"] = json!(u);
        }
        if let Some(i) = &self.icon {
            if i.starts_with(':') {
                body["icon_emoji"] = json!(i);
            } else {
                body["icon_url"] = json!(i);
            }
        }
        body
    }
}

#[async_trait]
impl Notifier for Slack {
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
            .json(&self.payload(event))
            .send()
            .await?;
        check(resp).await?;
        Ok(())
    }
}
