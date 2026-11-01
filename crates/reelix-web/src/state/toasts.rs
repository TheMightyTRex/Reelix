//! Transient notifications shown in the corner of the screen.

use std::time::Duration;

use leptos::prelude::*;

const AUTO_DISMISS: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
    Info,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
}

#[derive(Clone, Copy)]
pub struct Toasts {
    pub items: RwSignal<Vec<Toast>>,
    next_id: StoredValue<u64>,
}

impl Toasts {
    pub fn new() -> Self {
        Self {
            items: RwSignal::new(Vec::new()),
            next_id: StoredValue::new(1),
        }
    }

    pub fn expect() -> Self {
        expect_context()
    }

    pub fn push(&self, kind: ToastKind, message: impl Into<String>) {
        let id = self.next_id.get_value();
        self.next_id.set_value(id + 1);
        self.items.update(|items| {
            items.push(Toast {
                id,
                kind,
                message: message.into(),
            });
            // Keep the stack short; the oldest toasts go first.
            if items.len() > 5 {
                items.remove(0);
            }
        });
        // Errors stay until dismissed so there's time to read them (WCAG 2.2.1).
        if kind != ToastKind::Error {
            let this = *self;
            set_timeout(move || this.dismiss(id), AUTO_DISMISS);
        }
    }

    pub fn success(&self, message: impl Into<String>) {
        self.push(ToastKind::Success, message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.push(ToastKind::Error, message);
    }

    pub fn info(&self, message: impl Into<String>) {
        self.push(ToastKind::Info, message);
    }

    pub fn dismiss(&self, id: u64) {
        // The signal may already be disposed if the app unmounted.
        if self
            .items
            .try_with_untracked(|items| items.iter().any(|t| t.id == id))
            == Some(true)
        {
            self.items.update(|items| items.retain(|t| t.id != id));
        }
    }
}
