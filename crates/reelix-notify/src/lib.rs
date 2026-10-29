//! Notification adapters: Discord, Slack, Telegram, email, Pushover, Gotify, ntfy, Apprise, Matrix, webhook, script
//!
//! Every implementation is described by a [`ProviderSchema`] and built from a validated
//! JSON settings object via [`build`]. Each settings object carries an `events` list (the
//! subscription), surfaced as [`Notifier::supported_events`].

use std::time::Duration;

use async_trait::async_trait;
use reelix_core::provider::ProviderSchema;

mod common;
pub mod error;
pub mod event;
pub mod message;
pub mod samples;

pub mod apprise;
pub mod discord;
pub mod email;
pub mod gotify;
pub mod matrix;
pub mod ntfy;
pub mod pushover;
pub mod script;
pub mod slack;
pub mod telegram;
pub mod webhook;

pub use error::NotifyError;
pub use event::{
    DeleteReason, EpisodeSummary, EventKind, EventMask, FileSummary, HealthLevel, HealthSummary,
    IssueSummary, IssueType, MediaSummary, NotificationEvent, ReleaseSummary, RenamedFile, RequestStatus,
    RequestSummary, UpdateSummary,
};
pub use message::{Message, Severity};

/// A configured notification target.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Implementation id (matches [`ProviderSchema::implementation`]).
    fn implementation(&self) -> &'static str;

    /// Events this instance is subscribed to (from the `events` setting).
    fn supported_events(&self) -> EventMask;

    /// Delivers one event. Callers should check [`Notifier::wants`] first.
    async fn notify(&self, event: &NotificationEvent) -> Result<(), NotifyError>;

    /// Sends a test notification.
    async fn test(&self) -> Result<(), NotifyError> {
        self.notify(&NotificationEvent::test()).await
    }

    /// Whether this instance should receive `event` (test events always pass).
    fn wants(&self, event: &NotificationEvent) -> bool {
        let kind = event.kind();
        kind == EventKind::Test || self.supported_events().contains(kind)
    }
}

/// Shared dependencies handed to every notifier at build time.
#[derive(Debug, Clone)]
pub struct BuildContext {
    pub http: reqwest::Client,
    /// Display name used as sender/username where the service supports it.
    pub app_name: String,
    /// Timeout for custom scripts and SMTP sessions.
    pub timeout: Duration,
}

impl BuildContext {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            app_name: "Reelix".into(),
            timeout: Duration::from_secs(30),
        }
    }
}

/// One schema per implementation, in display order.
pub fn schemas() -> Vec<ProviderSchema> {
    vec![
        discord::schema(),
        slack::schema(),
        telegram::schema(),
        email::schema(),
        pushover::schema(),
        gotify::schema(),
        ntfy::schema(),
        apprise::schema(),
        matrix::schema(),
        webhook::schema(),
        script::schema(),
    ]
}

/// Builds a notifier from its implementation id and settings object.
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn Notifier>, NotifyError> {
    Ok(match implementation {
        discord::ID => Box::new(discord::Discord::from_settings(settings, ctx)?),
        slack::ID => Box::new(slack::Slack::from_settings(settings, ctx)?),
        telegram::ID => Box::new(telegram::Telegram::from_settings(settings, ctx)?),
        email::ID => Box::new(email::Email::from_settings(settings, ctx)?),
        pushover::ID => Box::new(pushover::Pushover::from_settings(settings, ctx)?),
        gotify::ID => Box::new(gotify::Gotify::from_settings(settings, ctx)?),
        ntfy::ID => Box::new(ntfy::Ntfy::from_settings(settings, ctx)?),
        apprise::ID => Box::new(apprise::Apprise::from_settings(settings, ctx)?),
        matrix::ID => Box::new(matrix::Matrix::from_settings(settings, ctx)?),
        webhook::ID => Box::new(webhook::Webhook::from_settings(settings, ctx)?),
        script::ID => Box::new(script::Script::from_settings(settings, ctx)?),
        other => return Err(NotifyError::UnknownImplementation(other.into())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_unique_and_have_events() {
        let s = schemas();
        let mut ids: Vec<_> = s.iter().map(|s| s.implementation.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 11);
        for schema in &s {
            assert!(schema.fields.iter().any(|f| f.name == "events"));
        }
    }

    #[test]
    fn unknown_implementation() {
        let ctx = BuildContext::new(reqwest::Client::new());
        let err = build("carrier-pigeon", &serde_json::json!({}), &ctx)
            .err()
            .unwrap();
        assert!(matches!(err, NotifyError::UnknownImplementation(_)));
    }
}
