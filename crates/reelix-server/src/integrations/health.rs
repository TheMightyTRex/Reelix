//! `health_watch`: notifies `HealthIssue` when a warning/error appears and `HealthRestored`
//! when it clears. Previous findings are kept in memory (the first run reports everything
//! currently failing).

use std::collections::BTreeMap;
use std::sync::Mutex;

use reelix_api_types::{HealthCheck, HealthLevel};
use reelix_notify::{HealthLevel as NotifyLevel, HealthSummary, NotificationEvent};

/// Remembers the last set of failing checks.
#[derive(Debug, Default)]
pub struct HealthWatch {
    previous: Mutex<BTreeMap<(String, String), HealthCheck>>,
}

fn summary(c: &HealthCheck) -> HealthSummary {
    HealthSummary {
        source: c.source.clone(),
        level: match c.level {
            HealthLevel::Error => NotifyLevel::Error,
            HealthLevel::Warning => NotifyLevel::Warning,
            HealthLevel::Ok => NotifyLevel::Notice,
        },
        message: c.message.clone(),
        wiki_url: None,
    }
}

impl HealthWatch {
    /// Compares `checks` with the previous run; returns the notifications to send.
    pub fn update(&self, checks: Vec<HealthCheck>) -> Vec<NotificationEvent> {
        let current: BTreeMap<(String, String), HealthCheck> = checks
            .into_iter()
            .filter(|c| c.level != HealthLevel::Ok)
            .map(|c| ((c.source.clone(), c.message.clone()), c))
            .collect();
        let mut prev = self.previous.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::new();
        for (key, c) in &current {
            match prev.get(key) {
                Some(p) if p.level == c.level => {}
                _ => out.push(NotificationEvent::HealthIssue(summary(c))),
            }
        }
        for (key, c) in prev.iter() {
            if !current.contains_key(key) {
                out.push(NotificationEvent::HealthRestored(summary(c)));
            }
        }
        *prev = current;
        out
    }
}
