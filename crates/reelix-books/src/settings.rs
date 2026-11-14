//! Book settings: naming templates, metadata/cover writing and Calibre.

use std::path::Path;

use reelix_api_types::books::BookSettingsDto;

use crate::error::{BooksError, Result};
use crate::naming::unknown_tokens;
use crate::{BooksService, keys};

/// Validation errors of `s` (`field: message`).
fn errors(s: &BookSettingsDto) -> Vec<String> {
    let n = &s.naming;
    let mut out = Vec::new();
    for (field, tpl) in [
        ("author_folder", &n.author_folder),
        ("ebook_file", &n.ebook_file),
        ("audiobook_folder", &n.audiobook_folder),
        ("audiobook_file", &n.audiobook_file),
        ("audiobook_part_file", &n.audiobook_part_file),
    ] {
        if tpl.trim().is_empty() {
            out.push(format!("naming.{field}: must not be empty"));
            continue;
        }
        let unknown = unknown_tokens(tpl);
        if !unknown.is_empty() {
            out.push(format!("naming.{field}: unknown token(s) {}", unknown.join(", ")));
        }
    }
    if !n.audiobook_part_file.to_ascii_lowercase().contains("{part") {
        out.push("naming.audiobook_part_file: must contain {Part}".into());
    }
    if let Some(e) = crate::audiobook::settings_errors(&s.audiobook_metadata) {
        out.push(e);
    }
    if s.calibre.enabled {
        match s.calibre.library_path.as_deref().map(str::trim) {
            Some(p) if Path::new(p).is_absolute() => {}
            _ => out.push("calibre.library_path: an absolute path is required".into()),
        }
    }
    out
}

impl BooksService {
    pub async fn settings(&self) -> Result<BookSettingsDto> {
        Ok(self
            .db
            .settings()
            .get::<BookSettingsDto>(keys::SETTINGS)
            .await?
            .unwrap_or_default())
    }

    /// Validates the templates and Calibre settings, then stores them.
    pub async fn set_settings(&self, mut s: BookSettingsDto) -> Result<BookSettingsDto> {
        s.audiobook_metadata.region = s.audiobook_metadata.region.trim().to_ascii_lowercase();
        if let Some(e) = errors(&s).into_iter().next() {
            return Err(BooksError::Validation(e));
        }
        if s.calibre.calibredb_path.trim().is_empty() {
            s.calibre.calibredb_path = "calibredb".into();
        }
        s.calibre.library_path = s
            .calibre
            .library_path
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        s.metadata_language = s
            .metadata_language
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        self.db.settings().set(keys::SETTINGS, &s).await?;
        self.emit("settings", "updated", 0);
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_valid() {
        assert!(errors(&BookSettingsDto::default()).is_empty());
        let mut s = BookSettingsDto::default();
        s.naming.ebook_file = "{Book Title} {Episode Title}".into();
        s.naming.audiobook_part_file = "{Book Title}".into();
        s.calibre.enabled = true;
        let e = errors(&s);
        assert_eq!(e.len(), 3, "{e:?}");
    }
}
