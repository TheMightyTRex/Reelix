//! Radio settings (`radio.settings`): the Audiobookshelf podcast library, its folder as
//! Audiobookshelf and as Reelix see it, the default follow choice and the sources.

use std::path::Path;

use reelix_api_types::radio::RadioSettingsDto;
use reelix_core::provider::SECRET_MASK;

use crate::error::{RadioError, Result};
use crate::{RadioService, keys};

/// Upper bound of [`RadioSettingsDto::max_lookups_per_refresh`].
const MAX_LOOKUPS: u32 = 1000;

/// Prefix of an encrypted settings value.
const ENC_PREFIX: &str = "enc:";

impl RadioService {
    /// The settings with credentials decrypted (internal use; the API reads
    /// [`settings_masked`](Self::settings_masked)). Every read also hands them to the
    /// providers ([`crate::RadioProvider::configure`]).
    pub async fn settings(&self) -> Result<RadioSettingsDto> {
        let mut s = self
            .db
            .settings()
            .get::<RadioSettingsDto>(keys::SETTINGS)
            .await?
            .unwrap_or_default();
        if let Some(stored) = s.podcastindex_api_secret.take() {
            s.podcastindex_api_secret = self.open_secret(&stored);
        }
        self.providers.configure(&s);
        Ok(s)
    }

    /// The settings for display: the Podcast Index secret is masked.
    pub async fn settings_masked(&self) -> Result<RadioSettingsDto> {
        Ok(mask(self.settings().await?))
    }

    fn open_secret(&self, stored: &str) -> Option<String> {
        let Some(enc) = stored.strip_prefix(ENC_PREFIX) else {
            return Some(stored.to_owned()).filter(|s| !s.is_empty());
        };
        match self.secrets.get().map(|b| b.decrypt(enc)) {
            Some(Ok(plain)) => Some(plain),
            Some(Err(e)) => {
                tracing::warn!(error = %e, "cannot decrypt the saved Podcast Index secret (was the secret key changed?)");
                None
            }
            None => {
                tracing::warn!("no secret key attached; the saved Podcast Index secret cannot be read");
                None
            }
        }
    }

    /// Normalises and checks the settings (the Reelix-side folder must exist and be
    /// writable), then stores them (credentials encrypted). Returns them masked.
    pub async fn set_settings(&self, mut s: RadioSettingsDto) -> Result<RadioSettingsDto> {
        if s.podcastindex_api_secret.as_deref() == Some(SECRET_MASK) {
            s.podcastindex_api_secret = self.settings().await?.podcastindex_api_secret;
        }
        let trim = |v: &mut Option<String>| {
            *v = v
                .as_deref()
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned);
        };
        trim(&mut s.abs_library_id);
        trim(&mut s.abs_library_folder);
        trim(&mut s.library_folder);
        trim(&mut s.country);
        trim(&mut s.language);
        trim(&mut s.podcastindex_api_key);
        trim(&mut s.podcastindex_api_secret);
        if let Some(f) = &s.abs_library_folder
            && !looks_absolute(f)
        {
            return Err(RadioError::Validation(format!(
                "the folder as Audiobookshelf sees it must be a full path, like /podcasts (got {f})"
            )));
        }
        if let Some(f) = s.library_folder.clone() {
            tokio::task::spawn_blocking(move || check_folder(Path::new(&f)))
                .await
                .map_err(|e| RadioError::Internal(e.to_string()))?
                .map_err(RadioError::Validation)?;
        }
        if let Some(c) = &s.country {
            if c.len() != 2 || !c.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(RadioError::Validation(format!(
                    "country must be a two-letter code such as GB or US (got {c})"
                )));
            }
            s.country = Some(c.to_ascii_uppercase());
        }
        if let Some(l) = &s.language {
            if !(2..=3).contains(&l.len()) || !l.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(RadioError::Validation(format!(
                    "language must be a two-letter code such as en or de (got {l})"
                )));
            }
            s.language = Some(l.to_ascii_lowercase());
        }
        let mut providers: Vec<String> = Vec::new();
        for p in s
            .enabled_providers
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
        {
            if self.providers.get(p).is_none() {
                return Err(RadioError::Validation(format!("unknown radio source {p}")));
            }
            if !providers.iter().any(|x| x == p) {
                providers.push(p.to_owned());
            }
        }
        s.enabled_providers = providers;
        // Catch-up only for sources that have catch-up pages (RFX-96).
        let mut catchup: Vec<String> = Vec::new();
        for p in s
            .catchup_providers
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
        {
            let Some(provider) = self.providers.get(p) else {
                return Err(RadioError::Validation(format!("unknown radio source {p}")));
            };
            if !provider.has_catchup() {
                return Err(RadioError::Validation(format!(
                    "{} has no catch-up to fetch",
                    provider.name()
                )));
            }
            if !catchup.iter().any(|x| x == p) {
                catchup.push(p.to_owned());
            }
        }
        s.catchup_providers = catchup;
        if s.max_lookups_per_refresh == 0 {
            s.max_lookups_per_refresh = RadioSettingsDto::default().max_lookups_per_refresh;
        }
        s.max_lookups_per_refresh = s.max_lookups_per_refresh.min(MAX_LOOKUPS);
        let mut stored = s.clone();
        if let Some(secret) = &s.podcastindex_api_secret {
            let b = self.secrets.get().ok_or_else(|| {
                RadioError::Internal("no secret key is attached, so credentials cannot be saved".into())
            })?;
            let enc = b
                .encrypt(secret)
                .map_err(|e| RadioError::Internal(e.to_string()))?;
            stored.podcastindex_api_secret = Some(format!("{ENC_PREFIX}{enc}"));
        }
        self.db.settings().set(keys::SETTINGS, &stored).await?;
        self.providers.configure(&s);
        self.emit("settings", "updated", 0);
        Ok(mask(s))
    }
}

fn mask(mut s: RadioSettingsDto) -> RadioSettingsDto {
    if s.podcastindex_api_secret.is_some() {
        s.podcastindex_api_secret = Some(SECRET_MASK.to_owned());
    }
    s
}

/// `/x` or `C:\x` (the Audiobookshelf side may be another machine or container).
fn looks_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with("\\\\")
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Whether Reelix can use `dir` as the podcast library folder: an existing, writable
/// directory. The error is worded for the settings page and health checks.
pub fn check_folder(dir: &Path) -> std::result::Result<(), String> {
    let shown = dir.display();
    if !dir.is_absolute() {
        return Err(format!("the library folder must be a full path (got {shown})"));
    }
    if !dir.exists() {
        return Err(format!(
            "Reelix cannot see the library folder {shown}; check that it is mounted"
        ));
    }
    if !dir.is_dir() {
        return Err(format!("{shown} is a file, not a folder"));
    }
    let probe = dir.join(format!(".reelix-write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "Reelix cannot write to the library folder {shown} ({e}); check its permissions"
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn folders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(super::check_folder(dir.path()).is_ok());
        assert!(
            super::check_folder(&dir.path().join("missing"))
                .unwrap_err()
                .contains("mounted")
        );
        assert!(super::check_folder(std::path::Path::new("relative")).is_err());
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(super::check_folder(&file).unwrap_err().contains("not a folder"));
        assert!(super::looks_absolute("/podcasts"));
        assert!(super::looks_absolute("D:\\Podcasts"));
        assert!(!super::looks_absolute("podcasts"));
    }
}
