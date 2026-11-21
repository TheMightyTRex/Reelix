//! The subtitle service: settings, searching one file, the schedule, and the import listener.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reelix_api_types::subtitles::{
    OpenSubtitlesSettings, SUBTITLE_SECRET_MASK, SubtitleFileDto, SubtitleLanguage, SubtitleMediaKind,
    SubtitleSettings,
};
use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_db::secrets::SecretBox;
use reelix_db::{Db, NewJob, now_ms};
use reelix_jobs::JobQueue;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{Error, Result};
use crate::matching::{Candidate, MatchLevel, Source, choices, rank, wanted};
use crate::naming::{Variant, normalise_language, subtitle_path};
use crate::opensubtitles::{self, Credentials, OpenSubtitles};
use crate::store::{self, NewRecord, VideoFile};
use crate::stremio::{self, StremioAddon};
use crate::{kinds, srt};

const SETTINGS_KEY: &str = "subtitles";
const HOUR_MS: i64 = 3600 * 1000;
/// Search again after a day while a subtitle is missing.
pub const RETRY_MISSING_MS: i64 = 24 * HOUR_MS;
/// After a week while only a better match could come.
pub const RETRY_UPGRADE_MS: i64 = 7 * 24 * HOUR_MS;
/// After a month when everything is made for the file (or nothing more is wanted).
pub const RETRY_COMPLETE_MS: i64 = 30 * 24 * HOUR_MS;
pub const MAX_LANGUAGES: usize = 10;
pub const MAX_ADDONS: usize = 5;
/// Candidates tried for one language and variant before giving up until the next search.
const TRIES: usize = 3;

/// Limits; the defaults suit a small server.
#[derive(Debug, Clone)]
pub struct Options {
    pub opensubtitles_base: Url,
    /// Files searched in one scheduled run.
    pub files_per_run: u32,
    /// Largest subtitle file accepted.
    pub max_bytes: usize,
    /// Spacing between requests to one source.
    pub gap: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            opensubtitles_base: Url::parse(opensubtitles::DEFAULT_BASE).expect("valid default"),
            files_per_run: 50,
            max_bytes: 4 * 1024 * 1024,
            gap: Duration::from_secs(1),
        }
    }
}

/// What one file's search did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchReport {
    /// Saved: (language, variant, match level).
    pub saved: Vec<(String, Variant, MatchLevel)>,
    /// Turned down by the checks, with the reason.
    pub rejected: Vec<String>,
    /// Still missing after the search.
    pub missing: Vec<(String, Variant)>,
}

pub struct SubtitleService {
    db: Db,
    events: EventBus,
    secrets: SecretBox,
    http: reqwest::Client,
    options: Options,
    opensubtitles: OpenSubtitles,
    addons: Mutex<HashMap<String, Arc<StremioAddon>>>,
    queue: OnceLock<JobQueue>,
}

impl SubtitleService {
    pub fn new(
        db: Db,
        events: EventBus,
        secrets: SecretBox,
        http: reqwest::Client,
        options: Options,
    ) -> Self {
        let opensubtitles =
            OpenSubtitles::new(http.clone(), options.opensubtitles_base.clone()).with_gap(options.gap);
        Self {
            db,
            events,
            secrets,
            http,
            options,
            opensubtitles,
            addons: Mutex::new(HashMap::new()),
            queue: OnceLock::new(),
        }
    }

    /// Gives the service the job queue (server `attach` phase).
    pub fn set_queue(&self, queue: JobQueue) {
        let _ = self.queue.set(queue);
    }

    // ------------------------------------------------------------------ settings

    /// Settings with the secrets readable (internal use).
    pub async fn settings(&self) -> Result<SubtitleSettings> {
        let mut s = self
            .db
            .settings()
            .get::<SubtitleSettings>(SETTINGS_KEY)
            .await?
            .unwrap_or_default();
        for secret in [&mut s.opensubtitles.api_key, &mut s.opensubtitles.password] {
            if !secret.is_empty() {
                *secret = self.secrets.decrypt(secret)?;
            }
        }
        Ok(s)
    }

    /// Settings for display: the API key and password are masked.
    pub async fn settings_masked(&self) -> Result<SubtitleSettings> {
        Ok(mask(self.settings().await?))
    }

    /// Checks, tidies and saves the settings; answers them masked.
    pub async fn set_settings(&self, s: SubtitleSettings) -> Result<SubtitleSettings> {
        let current = self.settings().await?;
        let s = validate(s, &current)?;
        let mut stored = s.clone();
        for secret in [
            &mut stored.opensubtitles.api_key,
            &mut stored.opensubtitles.password,
        ] {
            if !secret.is_empty() {
                *secret = self.secrets.encrypt(secret)?;
            }
        }
        self.db.settings().set(SETTINGS_KEY, &stored).await?;
        if s.languages != current.languages || s.stremio_addons != current.stremio_addons {
            // New wants: look at every file again on the next run.
            store::clear_searches(&self.db).await?;
        }
        self.addons.lock().await.clear();
        self.events.publish(DomainEvent::EntityChanged {
            entity: "settings".into(),
            action: "updated".into(),
            id: 0,
            user_id: None,
        });
        Ok(mask(s))
    }

    // ------------------------------------------------------------------ queries

    pub async fn records(&self, kind: SubtitleMediaKind, file_id: i64) -> Result<Vec<SubtitleFileDto>> {
        if store::video_file(&self.db, kind, file_id).await?.is_none() {
            return Err(Error::NotFound(format!("no {} file {file_id}", kind.as_str())));
        }
        store::records(&self.db, kind, file_id).await
    }

    /// Queues a search for one file now (`subtitles_search` with the file).
    pub async fn queue_search(&self, kind: SubtitleMediaKind, file_id: i64) -> Result<Option<i64>> {
        if !self.settings().await?.enabled {
            return Err(Error::Off);
        }
        if store::video_file(&self.db, kind, file_id).await?.is_none() {
            return Err(Error::NotFound(format!("no {} file {file_id}", kind.as_str())));
        }
        let job = NewJob::new(
            kinds::SEARCH,
            serde_json::json!({ "kind": kind.as_str(), "file_id": file_id }),
        )
        .dedupe(format!("subtitles:{}:{file_id}", kind.as_str()));
        self.enqueue(job).await
    }

    async fn enqueue(&self, job: NewJob) -> Result<Option<i64>> {
        match self.queue.get() {
            Some(q) => Ok(q.enqueue(job).await?),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------ import listener

    /// Listens for new film and episode files and puts them at the front of the schedule.
    /// Nothing happens while subtitles are off or searching after an import is.
    pub fn spawn_listener(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let svc = self.clone();
        let mut rx = self.events.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if let Err(e) = svc.on_event(&ev).await {
                            tracing::warn!(error = %e, "subtitles: could not note a new file");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            missed = n,
                            "subtitles listener lagged; the schedule covers the rest"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Handles one event: a created film or episode file is marked due and a run is queued.
    pub async fn on_event(&self, ev: &DomainEvent) -> Result<bool> {
        let DomainEvent::EntityChanged {
            entity, action, id, ..
        } = ev
        else {
            return Ok(false);
        };
        let kind = match (entity.as_str(), action.as_str()) {
            ("movie_file", "created") => SubtitleMediaKind::Movie,
            ("episode_file", "created") => SubtitleMediaKind::Episode,
            _ => return Ok(false),
        };
        let s = self.settings().await?;
        if !s.enabled || !s.fetch_after_import {
            return Ok(false);
        }
        store::mark_due_now(&self.db, kind, *id).await?;
        // Same key as the scheduled task, so a run already waiting takes it.
        self.enqueue(
            NewJob::new(kinds::SEARCH, serde_json::json!({})).dedupe(format!("task:{}", kinds::SEARCH)),
        )
        .await?;
        Ok(true)
    }

    // ------------------------------------------------------------------ searching

    /// A scheduled run: new imports first, then files due a search, at most `files_per_run`.
    /// New imports that arrive during the run are taken too. Answers the files searched.
    pub async fn run_due(&self) -> Result<usize> {
        let s = self.settings().await?;
        if !s.enabled || (!s.search_missing && !s.fetch_after_import) {
            return Ok(0);
        }
        let mut searched = 0usize;
        let mut seen = std::collections::HashSet::new();
        loop {
            let due = store::due(&self.db, now_ms(), self.options.files_per_run).await?;
            let mut progressed = false;
            for (kind, id) in due {
                if searched >= self.options.files_per_run as usize || !seen.insert((kind, id)) {
                    continue;
                }
                // Without the schedule, only new imports (due now) are searched.
                if !s.search_missing && !self.is_new(kind, id).await? {
                    continue;
                }
                progressed = true;
                searched += 1;
                match self.search_with(&s, kind, id).await {
                    Ok(_) => {}
                    Err(Error::QuotaExhausted) => {}
                    Err(e) => {
                        tracing::warn!(kind = kind.as_str(), id, error = %e, "subtitle search failed");
                        let _ = store::mark_searched(&self.db, kind, id, now_ms() + RETRY_MISSING_MS).await;
                    }
                }
            }
            if !progressed || searched >= self.options.files_per_run as usize {
                break;
            }
        }
        Ok(searched)
    }

    async fn is_new(&self, kind: SubtitleMediaKind, id: i64) -> Result<bool> {
        let next: Option<i64> = sqlx::query_scalar(
            "SELECT next_search_at FROM subtitle_searches WHERE media_kind = ? AND file_id = ?",
        )
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(next == Some(0))
    }

    /// Searches one file now, whatever the schedule says.
    pub async fn search_file(&self, kind: SubtitleMediaKind, file_id: i64) -> Result<SearchReport> {
        let s = self.settings().await?;
        if !s.enabled {
            return Err(Error::Off);
        }
        self.search_with(&s, kind, file_id).await
    }

    async fn search_with(
        &self,
        s: &SubtitleSettings,
        kind: SubtitleMediaKind,
        file_id: i64,
    ) -> Result<SearchReport> {
        let mut report = SearchReport::default();
        let video = store::video_file(&self.db, kind, file_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no {} file {file_id}", kind.as_str())))?;
        let existing = store::records(&self.db, kind, file_id).await?;
        let current = |lang: &str, v: Variant| {
            existing
                .iter()
                .find(|r| r.language == lang && r.variant == v.as_str())
                .and_then(|r| MatchLevel::parse(&r.match_level))
        };

        // What to look for: missing ones, and ones not yet made for the file when upgrading.
        // A subtitle someone else saved under the same name counts as there.
        let mut todo: Vec<(String, Variant, Option<MatchLevel>)> = Vec::new();
        for lang in &s.languages {
            for v in variants(lang) {
                let cur = current(&lang.code, v);
                if !wanted(cur, s.upgrade) {
                    continue;
                }
                let target = subtitle_path(&video.path, &lang.code, v);
                if cur.is_none() && tokio::fs::try_exists(&target).await.unwrap_or(false) {
                    continue;
                }
                todo.push((lang.code.clone(), v, cur));
            }
        }
        if todo.is_empty() {
            store::mark_searched(&self.db, kind, file_id, now_ms() + RETRY_COMPLETE_MS).await?;
            return Ok(report);
        }
        if !tokio::fs::try_exists(&video.path).await.unwrap_or(false) {
            return Err(Error::NotFound(format!(
                "the video file is not on disk: {}",
                video.path.display()
            )));
        }

        let hash = {
            let p = video.path.clone();
            tokio::task::spawn_blocking(move || crate::hash::file_hash(&p))
                .await
                .map_err(|e| Error::Source(e.to_string()))?
                .map(|(h, _)| h)
                .map_err(|e| tracing::debug!(error = %e, "could not hash the video file"))
                .ok()
        };
        let languages: Vec<String> = {
            let mut l: Vec<String> = todo.iter().map(|(l, _, _)| l.clone()).collect();
            l.dedup();
            l
        };
        let mut candidates = self.find(s, &video, hash.as_deref(), &languages).await;
        rank(&mut candidates, video.release.as_deref());

        let mut quota_out = false;
        for (lang, variant, cur) in &todo {
            let mut saved = false;
            for c in choices(&candidates, lang, *variant, *cur, s.upgrade).take(TRIES) {
                if quota_out && c.source == Source::OpenSubtitles {
                    continue;
                }
                let bytes = match self.download(s, c).await {
                    Ok(b) => b,
                    Err(Error::QuotaExhausted) => {
                        quota_out = true;
                        continue;
                    }
                    Err(e) => {
                        tracing::info!(source = c.source.name(), error = %e, "subtitle download failed");
                        continue;
                    }
                };
                let checked = match srt::check(&bytes, video.duration) {
                    Ok(c) => c,
                    Err(why) => {
                        report.rejected.push(format!(
                            "{lang} {} from {}: {why}",
                            variant.as_str(),
                            c.source.name()
                        ));
                        continue;
                    }
                };
                let target = subtitle_path(&video.path, lang, *variant);
                write_atomically(&target, &checked.bytes).await?;
                store::save(
                    &self.db,
                    &NewRecord {
                        kind,
                        file_id,
                        language: lang,
                        variant: *variant,
                        path: &target,
                        source: c.source.name(),
                        source_id: &c.source_id,
                        level: c.level,
                        release: c.release.as_deref(),
                    },
                )
                .await?;
                tracing::info!(
                    file = %video.path.display(), language = %lang, variant = variant.as_str(),
                    level = c.level.as_str(), source = c.source.name(), "subtitle saved"
                );
                report.saved.push((lang.clone(), *variant, c.level));
                saved = true;
                break;
            }
            if !saved && cur.is_none() {
                report.missing.push((lang.clone(), *variant));
            }
        }
        if !report.saved.is_empty() {
            self.events.publish(DomainEvent::EntityChanged {
                entity: "subtitle".into(),
                action: "updated".into(),
                id: file_id,
                user_id: None,
            });
        }

        let next = if !report.missing.is_empty() {
            RETRY_MISSING_MS
        } else if s.upgrade
            && todo.iter().any(|(l, v, _)| {
                !report
                    .saved
                    .iter()
                    .any(|(sl, sv, lvl)| sl == l && sv == v && *lvl == MatchLevel::Hash)
            })
        {
            RETRY_UPGRADE_MS
        } else {
            RETRY_COMPLETE_MS
        };
        store::mark_searched(&self.db, kind, file_id, now_ms() + next).await?;
        if quota_out {
            return Err(Error::QuotaExhausted);
        }
        Ok(report)
    }

    /// Asks every source; a source that fails is logged and left out.
    async fn find(
        &self,
        s: &SubtitleSettings,
        video: &VideoFile,
        hash: Option<&str>,
        languages: &[String],
    ) -> Vec<Candidate> {
        let mut out = Vec::new();
        if let Some(creds) = credentials(&s.opensubtitles) {
            let query = opensubtitles::Query {
                moviehash: hash.map(str::to_owned),
                imdb_id: video.imdb_id,
                tmdb_id: video.tmdb_id,
                episode: video.episode,
                title: Some(video.title.clone()),
                year: video.year,
                languages: languages.to_vec(),
            };
            // An episode without its season and episode numbers cannot be asked for by id.
            if video.kind == SubtitleMediaKind::Movie || video.episode.is_some() {
                match self.opensubtitles.search(&creds, &query).await {
                    Ok(resp) => {
                        out.extend(opensubtitles::candidates(&resp, &query, video.release.as_deref()))
                    }
                    Err(e) => tracing::warn!(error = %e, "OpenSubtitles search failed"),
                }
            }
        }
        if let Some(imdb) = video.imdb_tt()
            && (video.kind == SubtitleMediaKind::Movie || video.episode.is_some())
        {
            let req = stremio::Request {
                imdb_id: imdb,
                episode: video.episode,
                video_hash: hash.map(str::to_owned),
                video_size: Some(video.size),
                filename: video.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            };
            for (i, addr) in s.stremio_addons.iter().enumerate() {
                let addon = match self.addon(addr).await {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(addon = %addr, error = %e, "subtitle add-on address not usable");
                        continue;
                    }
                };
                match addon.subtitles(&req).await {
                    Ok(resp) => out.extend(stremio::candidates(&resp, i, languages)),
                    Err(e) => tracing::warn!(addon = %addon.base, error = %e, "subtitle add-on failed"),
                }
            }
        }
        out
    }

    async fn addon(&self, addr: &str) -> Result<Arc<StremioAddon>> {
        let mut addons = self.addons.lock().await;
        if let Some(a) = addons.get(addr) {
            return Ok(a.clone());
        }
        let a = Arc::new(
            StremioAddon::new(self.http.clone(), stremio::addon_base(addr)?).with_gap(self.options.gap),
        );
        addons.insert(addr.to_owned(), a.clone());
        Ok(a)
    }

    async fn download(&self, s: &SubtitleSettings, c: &Candidate) -> Result<Vec<u8>> {
        match c.source {
            Source::OpenSubtitles => {
                let creds = credentials(&s.opensubtitles)
                    .ok_or_else(|| Error::Settings("OpenSubtitles is not set up".into()))?;
                self.opensubtitles
                    .download(&creds, &c.source_id, self.options.max_bytes)
                    .await
            }
            Source::Stremio(i) => {
                let addr = s
                    .stremio_addons
                    .get(i)
                    .ok_or_else(|| Error::Settings("add-on removed".into()))?;
                self.addon(addr)
                    .await?
                    .download(&c.source_id, self.options.max_bytes)
                    .await
            }
        }
    }
}

fn credentials(os: &OpenSubtitlesSettings) -> Option<Credentials> {
    (os.enabled && !os.api_key.trim().is_empty()).then(|| Credentials {
        api_key: os.api_key.clone(),
        username: os.username.clone(),
        password: os.password.clone(),
    })
}

fn variants(lang: &SubtitleLanguage) -> Vec<Variant> {
    [
        (lang.normal, Variant::Normal),
        (lang.sdh, Variant::Sdh),
        (lang.forced, Variant::Forced),
    ]
    .into_iter()
    .filter_map(|(on, v)| on.then_some(v))
    .collect()
}

fn mask(mut s: SubtitleSettings) -> SubtitleSettings {
    for secret in [&mut s.opensubtitles.api_key, &mut s.opensubtitles.password] {
        if !secret.is_empty() {
            *secret = SUBTITLE_SECRET_MASK.into();
        }
    }
    s
}

/// Checks and tidies settings from the API; a masked secret keeps the stored one.
pub fn validate(mut s: SubtitleSettings, current: &SubtitleSettings) -> Result<SubtitleSettings> {
    if s.opensubtitles.api_key == SUBTITLE_SECRET_MASK {
        s.opensubtitles.api_key = current.opensubtitles.api_key.clone();
    }
    if s.opensubtitles.password == SUBTITLE_SECRET_MASK {
        s.opensubtitles.password = current.opensubtitles.password.clone();
    }
    s.opensubtitles.api_key = s.opensubtitles.api_key.trim().to_owned();
    s.opensubtitles.username = s.opensubtitles.username.trim().to_owned();

    let mut languages: Vec<SubtitleLanguage> = Vec::new();
    for l in s.languages {
        let code = normalise_language(&l.code).ok_or_else(|| {
            Error::Settings(format!(
                "{} is not a language code: use two letters, such as en, or with a region, such as pt-BR",
                l.code.trim()
            ))
        })?;
        if languages.iter().any(|x| x.code == code) {
            continue;
        }
        if !(l.normal || l.sdh || l.forced) {
            return Err(Error::Settings(format!(
                "choose at least one of normal, SDH or forced for {code}"
            )));
        }
        languages.push(SubtitleLanguage { code, ..l });
    }
    if languages.len() > MAX_LANGUAGES {
        return Err(Error::Settings(format!("at most {MAX_LANGUAGES} languages")));
    }
    s.languages = languages;

    let mut addons: Vec<String> = Vec::new();
    for a in s
        .stremio_addons
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
    {
        let base = stremio::addon_base(a)?.to_string();
        if !addons.contains(&base) {
            addons.push(base);
        }
    }
    if addons.len() > MAX_ADDONS {
        return Err(Error::Settings(format!("at most {MAX_ADDONS} add-ons")));
    }
    s.stremio_addons = addons;

    if s.opensubtitles.enabled && s.opensubtitles.api_key.is_empty() {
        return Err(Error::Settings("OpenSubtitles needs an API key".into()));
    }
    if !s.opensubtitles.username.is_empty() && s.opensubtitles.password.is_empty() {
        return Err(Error::Settings(
            "enter the OpenSubtitles password for this username".into(),
        ));
    }
    if s.enabled {
        if s.languages.is_empty() {
            return Err(Error::Settings("choose at least one language".into()));
        }
        if !s.opensubtitles.enabled && s.stremio_addons.is_empty() {
            return Err(Error::Settings(
                "activate OpenSubtitles or add a Stremio subtitle add-on".into(),
            ));
        }
    }
    Ok(s)
}

/// Writes beside the video through a temporary file, so a player never reads half a subtitle.
async fn write_atomically(target: &Path, bytes: &[u8]) -> Result<()> {
    let tmp: PathBuf = target.with_extension("srt.reelix-part");
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(code: &str) -> SubtitleLanguage {
        SubtitleLanguage {
            code: code.into(),
            normal: true,
            sdh: false,
            forced: false,
        }
    }

    #[test]
    fn off_settings_need_nothing() {
        assert!(validate(SubtitleSettings::default(), &SubtitleSettings::default()).is_ok());
    }

    #[test]
    fn on_needs_a_language_and_a_source() {
        let on = SubtitleSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(validate(on.clone(), &on).is_err());
        let with_lang = SubtitleSettings {
            languages: vec![lang("en")],
            ..on.clone()
        };
        assert!(validate(with_lang.clone(), &on).is_err());
        let ok = SubtitleSettings {
            stremio_addons: vec!["https://addon.example/manifest.json".into()],
            ..with_lang
        };
        let saved = validate(ok, &on).unwrap();
        assert_eq!(saved.stremio_addons, ["https://addon.example/"]);
    }

    #[test]
    fn languages_are_tidied_and_checked() {
        let s = SubtitleSettings {
            languages: vec![lang(" EN"), lang("en"), lang("pt_br")],
            ..Default::default()
        };
        let v = validate(s, &SubtitleSettings::default()).unwrap();
        let codes: Vec<_> = v.languages.iter().map(|l| l.code.as_str()).collect();
        assert_eq!(codes, ["en", "pt-BR"]);
        let bad = SubtitleSettings {
            languages: vec![lang("english")],
            ..Default::default()
        };
        assert!(validate(bad, &SubtitleSettings::default()).is_err());
        let none = SubtitleSettings {
            languages: vec![SubtitleLanguage {
                normal: false,
                ..lang("en")
            }],
            ..Default::default()
        };
        assert!(validate(none, &SubtitleSettings::default()).is_err());
    }

    #[test]
    fn masked_secrets_keep_the_stored_ones() {
        let current = SubtitleSettings {
            opensubtitles: OpenSubtitlesSettings {
                enabled: true,
                api_key: "key".into(),
                username: "someone".into(),
                password: "pw".into(),
            },
            ..Default::default()
        };
        let sent = mask(current.clone());
        assert_eq!(sent.opensubtitles.api_key, SUBTITLE_SECRET_MASK);
        let v = validate(sent, &current).unwrap();
        assert_eq!(v.opensubtitles, current.opensubtitles);
    }
}
