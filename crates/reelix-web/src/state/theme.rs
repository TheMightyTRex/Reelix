//! Dark / light theme, persisted in `localStorage` and applied as `data-theme` on `<html>`.

use leptos::prelude::*;

const STORAGE_KEY: &str = "reelix.theme";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub mode: RwSignal<Mode>,
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

impl Theme {
    /// Reads the saved preference (dark by default) and keeps `<html data-theme>` in sync.
    pub fn init() -> Self {
        let saved = storage().and_then(|s| s.get_item(STORAGE_KEY).ok().flatten());
        let mode = if saved.as_deref() == Some("light") {
            Mode::Light
        } else {
            Mode::Dark
        };
        let theme = Self {
            mode: RwSignal::new(mode),
        };
        Effect::new(move |_| {
            let mode = theme.mode.get();
            if let Some(root) = document().document_element() {
                let _ = root.set_attribute("data-theme", mode.as_str());
            }
            if let Some(s) = storage() {
                let _ = s.set_item(STORAGE_KEY, mode.as_str());
            }
        });
        theme
    }

    pub fn expect() -> Self {
        expect_context()
    }

    pub fn toggle(&self) {
        self.mode.update(|m| {
            *m = match m {
                Mode::Dark => Mode::Light,
                Mode::Light => Mode::Dark,
            }
        });
    }
}
