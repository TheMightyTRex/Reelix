use serde::{Deserialize, Serialize};

use crate::jobs::JobStatus;

/// Domain events broadcast on the event bus and forwarded to WebSocket clients.
///
/// Serialized as `{ "type": "<name>", "data": { ... } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DomainEvent {
    #[serde(rename = "setup.completed")]
    SetupCompleted { user_id: i64 },

    #[serde(rename = "user.created")]
    UserCreated { user_id: i64, username: String },

    #[serde(rename = "job.queued")]
    JobQueued { id: i64, kind: String },

    #[serde(rename = "job.started")]
    JobStarted { id: i64, kind: String },

    #[serde(rename = "job.finished")]
    JobFinished {
        id: i64,
        kind: String,
        status: JobStatus,
        duration_ms: i64,
        error: Option<String>,
    },

    #[serde(rename = "task.updated")]
    TaskUpdated { kind: String },

    /// Generic change notification so UIs can refresh: `entity` is e.g. `movie`, `series`,
    /// `episode`, `indexer`, `download_client`, `request`, `issue`, `queue`; `action` is
    /// `created`, `updated`, `deleted` or a domain verb (`grabbed`, `imported`, …).
    #[serde(rename = "entity.changed")]
    EntityChanged {
        entity: String,
        action: String,
        id: i64,
        /// Owning user for user-scoped entities (requests, issues); `None` = admins only.
        user_id: Option<i64>,
    },
}

/// Who may receive an event over the WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Admins only (system internals).
    Admin,
    /// A single user, plus admins.
    User(i64),
    /// Every authenticated user.
    All,
}

impl DomainEvent {
    pub fn audience(&self) -> Audience {
        match self {
            Self::SetupCompleted { .. } => Audience::All,
            Self::EntityChanged {
                user_id: Some(id), ..
            } => Audience::User(*id),
            Self::EntityChanged { user_id: None, .. } => Audience::Admin,
            Self::UserCreated { .. }
            | Self::JobQueued { .. }
            | Self::JobStarted { .. }
            | Self::JobFinished { .. }
            | Self::TaskUpdated { .. } => Audience::Admin,
        }
    }

    /// The wire name of the event (the `type` field).
    pub fn name(&self) -> &'static str {
        match self {
            Self::SetupCompleted { .. } => "setup.completed",
            Self::UserCreated { .. } => "user.created",
            Self::JobQueued { .. } => "job.queued",
            Self::JobStarted { .. } => "job.started",
            Self::JobFinished { .. } => "job.finished",
            Self::TaskUpdated { .. } => "task.updated",
            Self::EntityChanged { .. } => "entity.changed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_type_and_data() {
        let ev = DomainEvent::JobStarted {
            id: 7,
            kind: "housekeeping".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "job.started");
        assert_eq!(json["data"]["id"], 7);
        assert_eq!(ev.name(), "job.started");
        let back: DomainEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }
}
