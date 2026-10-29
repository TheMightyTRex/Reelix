//! Music settings (`music.settings`): naming, tag/cover writing and release preferences.

use reelix_api_types::music::MusicSettingsDto;

use crate::error::Result;
use crate::{MusicService, keys};

impl MusicService {
    pub async fn settings(&self) -> Result<MusicSettingsDto> {
        Ok(self
            .db
            .settings()
            .get::<MusicSettingsDto>(keys::SETTINGS)
            .await?
            .unwrap_or_default())
    }

    /// Validates the naming templates, normalises the preference lists and stores the settings.
    pub async fn set_settings(&self, mut s: MusicSettingsDto) -> Result<MusicSettingsDto> {
        crate::naming::validate(&s.naming)?;
        if s.naming
            .multi_disc_folder
            .as_deref()
            .is_some_and(|f| f.trim().is_empty())
        {
            s.naming.multi_disc_folder = None;
        }
        let clean = |v: &mut Vec<String>, upper: bool| {
            let mut out: Vec<String> = Vec::new();
            for x in v.iter().map(|x| x.trim()).filter(|x| !x.is_empty()) {
                let x = if upper {
                    x.to_ascii_uppercase()
                } else {
                    x.to_owned()
                };
                if !out.iter().any(|o| o.eq_ignore_ascii_case(&x)) {
                    out.push(x);
                }
            }
            *v = out;
        };
        clean(&mut s.preferred_release_countries, true);
        clean(&mut s.preferred_formats, false);
        self.db.settings().set(keys::SETTINGS, &s).await?;
        self.emit("settings", "updated", 0);
        Ok(s)
    }
}
