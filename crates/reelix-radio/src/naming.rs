//! Naming of filed episodes (`radio.naming`), rendered with
//! [`reelix_import::naming::render_custom`] like the book and music tokens.
//!
//! Tokens: `{Show Title}`, `{Show CleanTitle}`, `{Station Name}`, `{Series:00}`,
//! `{Episode:00}`, `{Episode Title}`, `{Air Date}`, `{Air Year}`, `{Quality Title}`. The
//! defaults give `{Show}/{Show} - S{series:00}E{episode:00} - {Title}.{ext}`, the same as
//! [`episode_file_name`](crate::episode_file_name).

use chrono::{Datelike, NaiveDate};
use reelix_api_types::radio::RadioNamingDto;
use reelix_import::naming::{NamingConfig, pad_number, render_custom, sanitize, unknown_custom_tokens};
use reelix_parser::Quality;

use crate::error::{RadioError, Result};
use crate::{RadioService, keys};

/// Every token key (lowercased, as the template parser reports them).
const TOKENS: &[&str] = &[
    "show title",
    "show cleantitle",
    "station name",
    "series",
    "episode",
    "episode title",
    "air date",
    "air year",
    "quality title",
];

/// Facts the tokens are rendered from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpisodeNaming {
    pub show_title: String,
    pub station: Option<String>,
    /// Filing series number (specials = 0).
    pub series: u32,
    /// `None` renders as 0.
    pub episode: Option<u32>,
    pub title: String,
    pub air_date: Option<NaiveDate>,
    pub quality: Option<Quality>,
}

impl EpisodeNaming {
    fn resolve(&self, key: &str, spec: Option<&str>) -> Option<String> {
        Some(match key {
            "show title" => self.show_title.trim().to_owned(),
            "show cleantitle" => reelix_import::naming::clean_title(&self.show_title),
            "station name" => self.station.clone().unwrap_or_default(),
            "series" => pad_number(self.series, spec.or(Some("00"))),
            "episode" => pad_number(self.episode.unwrap_or(0), spec.or(Some("00"))),
            "episode title" => self.title.trim().to_owned(),
            "air date" => self.air_date.map(|d| d.to_string()).unwrap_or_default(),
            "air year" => self.air_date.map(|d| d.year().to_string()).unwrap_or_default(),
            "quality title" => self.quality.map(|q| q.name().to_owned()).unwrap_or_default(),
            _ => return None,
        })
    }

    fn render(&self, template: &str, config: &NamingConfig) -> String {
        render_custom(template, &|k, s| self.resolve(k, s), config)
    }

    /// `{show folder}/{episode file}.{ext}` relative to the podcast library folder. Empty
    /// renders fall back to the show title and the default file name.
    pub fn path(&self, n: &RadioNamingDto, ext: &str) -> String {
        let config = NamingConfig::default();
        let mut folder = self.render(&n.show_folder, &config);
        if folder.is_empty() {
            folder = sanitize(self.show_title.trim(), &config);
        }
        let mut file = self.render(&n.episode_file, &config);
        if file.is_empty() {
            file = self.render(&RadioNamingDto::default().episode_file, &config);
        }
        let ext = ext.trim().trim_start_matches('.');
        if ext.is_empty() {
            format!("{folder}/{file}")
        } else {
            format!("{folder}/{file}.{ext}")
        }
    }
}

/// Problems with a naming template (empty, unknown tokens).
fn problems(n: &RadioNamingDto) -> Vec<String> {
    let mut out = Vec::new();
    for (field, value) in [("show_folder", &n.show_folder), ("episode_file", &n.episode_file)] {
        if value.trim().is_empty() {
            out.push(format!("{field}: must not be empty"));
            continue;
        }
        let unknown = unknown_custom_tokens(value, &|k| TOKENS.contains(&k));
        if !unknown.is_empty() {
            out.push(format!("{field}: unknown token(s) {}", unknown.join(", ")));
        }
    }
    if n.show_folder.contains('/') || n.show_folder.contains('\\') {
        out.push("show_folder: must be one folder (no slashes)".into());
    }
    out
}

impl RadioService {
    /// Naming of filed episodes (defaults when never saved).
    pub async fn naming(&self) -> Result<RadioNamingDto> {
        Ok(self
            .db
            .settings()
            .get::<RadioNamingDto>(keys::NAMING)
            .await?
            .unwrap_or_default())
    }

    /// Checks (non-empty, known tokens) and stores the naming templates.
    pub async fn set_naming(&self, mut n: RadioNamingDto) -> Result<RadioNamingDto> {
        n.show_folder = n.show_folder.trim().to_owned();
        n.episode_file = n.episode_file.trim().to_owned();
        let p = problems(&n);
        if !p.is_empty() {
            return Err(RadioError::Validation(p.join("; ")));
        }
        self.db.settings().set(keys::NAMING, &n).await?;
        self.emit("settings", "updated", 0);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> EpisodeNaming {
        EpisodeNaming {
            show_title: "The News Quiz".into(),
            station: Some("BBC Radio 4".into()),
            series: 20,
            episode: Some(3),
            title: "Episode 3".into(),
            air_date: NaiveDate::from_ymd_opt(2026, 11, 4),
            quality: Some(Quality::AudiobookMp3),
        }
    }

    #[test]
    fn defaults_match_episode_file_name() {
        let n = RadioNamingDto::default();
        assert_eq!(
            facts().path(&n, "mp3"),
            crate::episode_file_name("The News Quiz", 20, Some(3), "Episode 3", "mp3")
        );
        let special = EpisodeNaming {
            series: 0,
            episode: Some(1),
            title: "Christmas: Special".into(),
            ..facts()
        };
        assert_eq!(
            special.path(&n, ".m4a"),
            crate::episode_file_name("The News Quiz", 0, Some(1), "Christmas: Special", "m4a")
        );
    }

    #[test]
    fn custom_tokens() {
        let n = RadioNamingDto {
            show_folder: "{Station Name} - {Show Title}".into(),
            episode_file: "{Air Date} {Episode Title} [{Quality Title}]".into(),
            write_tags: true,
        };
        assert_eq!(
            facts().path(&n, "mp3"),
            "BBC Radio 4 - The News Quiz/2026-11-04 Episode 3 [Audiobook MP3].mp3"
        );
        assert!(problems(&n).is_empty());
        let bad = RadioNamingDto {
            show_folder: String::new(),
            episode_file: "{Show Title} {Bogus}".into(),
            write_tags: true,
        };
        let p = problems(&bad);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p[1].contains("{Bogus}"), "{p:?}");
    }
}
