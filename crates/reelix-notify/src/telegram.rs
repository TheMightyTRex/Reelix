//! Telegram Bot API `sendMessage` with HTML formatting.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::common::{self, Settings, check, schema as mk_schema, truncate};
use crate::{BuildContext, EventMask, Message, NotificationEvent, Notifier, NotifyError};

pub const ID: &str = "telegram";
const DEFAULT_API: &str = "https://api.telegram.org";

pub fn schema() -> ProviderSchema {
    mk_schema(
        ID,
        "Telegram",
        "Sends messages through a Telegram bot",
        Some("https://core.telegram.org/bots#how-do-i-create-a-bot"),
        vec![
            FieldSchema::new("bot_token", "Bot token", FieldKind::Secret).required(),
            FieldSchema::new("chat_id", "Chat ID", FieldKind::Text)
                .required()
                .help("User, group or channel id (e.g. -1001234567890 or @channel)"),
            FieldSchema::new("topic_id", "Topic ID", FieldKind::Number)
                .help("Forum topic (message thread) id")
                .advanced(),
            FieldSchema::new("silent", "Send silently", FieldKind::Bool).default_value(false),
            FieldSchema::new("include_links", "Include links", FieldKind::Bool).default_value(true),
            FieldSchema::new("api_url", "API URL", FieldKind::Url)
                .default_value(DEFAULT_API)
                .advanced(),
        ],
    )
}

#[derive(Deserialize)]
struct TgResponse {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<u16>,
}

/// Telegram bot notifier.
pub struct Telegram {
    http: reqwest::Client,
    api_url: String,
    bot_token: String,
    chat_id: String,
    topic_id: Option<i64>,
    silent: bool,
    include_links: bool,
    events: EventMask,
}

impl Telegram {
    pub fn from_settings(raw: &Value, ctx: &BuildContext) -> Result<Self, NotifyError> {
        let s = Settings::new(&schema(), raw)?;
        let bot_token = s.str("bot_token")?;
        if bot_token.contains('/') {
            return Err(NotifyError::settings("bot token must not contain '/'"));
        }
        Ok(Self {
            http: ctx.http.clone(),
            api_url: s.url("api_url")?,
            bot_token,
            chat_id: s.str("chat_id")?,
            topic_id: s.opt_i64("topic_id"),
            silent: s.bool("silent"),
            include_links: s.bool("include_links"),
            events: s.events()?,
        })
    }

    /// The `sendMessage` JSON body.
    pub fn payload(&self, event: &NotificationEvent) -> Value {
        let mut m = Message::from_event(event);
        if !self.include_links {
            m.url = None;
        }
        let mut text = m.html(true);
        // Telegram's limit is 4096 chars; leave headroom so we never cut inside a tag pair badly.
        truncate(&mut text, 4000);
        let mut body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_notification": self.silent,
            "link_preview_options": { "is_disabled": !self.include_links },
        });
        if let Some(t) = self.topic_id {
            body["message_thread_id"] = json!(t);
        }
        body
    }
}

#[async_trait]
impl Notifier for Telegram {
    fn implementation(&self) -> &'static str {
        ID
    }

    fn supported_events(&self) -> EventMask {
        self.events
    }

    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError> {
        let url = format!("{}/bot{}/sendMessage", self.api_url, self.bot_token);
        let resp = self.http.post(url).json(&self.payload(event)).send().await?;
        let status = resp.status();
        // Telegram reports errors as JSON `{ok:false, description}` with 4xx statuses.
        if status.is_client_error() && status.as_u16() != 429 {
            let r: Option<TgResponse> = common::json(resp).await.ok();
            let (code, desc) = r
                .map(|r| {
                    (
                        r.error_code.unwrap_or(status.as_u16()),
                        r.description.unwrap_or_default(),
                    )
                })
                .unwrap_or((status.as_u16(), String::new()));
            return Err(if code == 401 {
                NotifyError::Unauthorized(desc)
            } else {
                NotifyError::Api(format!("{code}: {desc}"))
            });
        }
        let r: TgResponse = common::json(check(resp).await?).await?;
        if !r.ok {
            return Err(NotifyError::Api(r.description.unwrap_or_default()));
        }
        Ok(())
    }
}
