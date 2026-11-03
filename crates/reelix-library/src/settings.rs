//! Naming and media-management settings.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use reelix_api_types::library::{MediaManagementSettings, NamingErrorDto, NamingPreviewDto, NamingSettings};
use reelix_core::ContentKind;
use reelix_import::files::{PathGuard, RecycleBin, delete_permanently};
use reelix_import::naming::{NamingConfig, validate_template};

use crate::convert::{naming_from_config, naming_to_config};
use crate::error::{LibraryError, Result};
use crate::{LibraryService, content_roots, keys};

impl LibraryService {
    pub async fn naming(&self) -> Result<NamingSettings> {
        Ok(naming_from_config(&self.naming_config().await?))
    }

    pub(crate) async fn naming_config(&self) -> Result<NamingConfig> {
        Ok(self
            .db
            .settings()
            .get::<NamingConfig>(keys::NAMING)
            .await?
            .unwrap_or_default())
    }

    /// Validates every template, then stores the settings.
    pub async fn set_naming(&self, s: NamingSettings) -> Result<NamingSettings> {
        let errors = naming_errors(&s);
        if let Some(e) = errors.first() {
            return Err(LibraryError::validation(format!("{}: {}", e.field, e.message)));
        }
        let cfg = naming_to_config(&s);
        self.db.settings().set(keys::NAMING, &cfg).await?;
        self.emit("settings", "updated", 0);
        Ok(naming_from_config(&cfg))
    }

    /// Sample names for `s` (or the stored settings), plus template errors.
    pub async fn naming_preview(&self, s: Option<NamingSettings>) -> Result<NamingPreviewDto> {
        let s = match s {
            Some(s) => s,
            None => self.naming().await?,
        };
        let errors = naming_errors(&s);
        let p = naming_to_config(&s).preview();
        Ok(NamingPreviewDto {
            movie_folder: p.movie_folder,
            movie_file: p.movie_file,
            series_folder: p.series_folder,
            season_folder: p.season_folder,
            standard_episode: p.standard_episode,
            multi_episode: p.multi_episode,
            daily_episode: p.daily_episode,
            anime_episode: p.anime_episode,
            errors,
        })
    }

    pub async fn media_management(&self) -> Result<MediaManagementSettings> {
        Ok(self
            .db
            .settings()
            .get::<MediaManagementSettings>(keys::MEDIA_MANAGEMENT)
            .await?
            .unwrap_or_default())
    }

    pub async fn set_media_management(
        &self,
        mut s: MediaManagementSettings,
    ) -> Result<MediaManagementSettings> {
        for (name, mode) in [("file_mode", &s.file_mode), ("dir_mode", &s.dir_mode)] {
            if let Some(m) = mode.as_deref().filter(|m| !m.trim().is_empty())
                && u32::from_str_radix(m.trim(), 8).map_or(true, |v| v > 0o7777)
            {
                return Err(LibraryError::validation(format!(
                    "{name}: {m:?} is not an octal mode"
                )));
            }
        }
        if let Some(p) = s.recycle_bin_path.as_deref().map(str::trim) {
            if p.is_empty() {
                s.recycle_bin_path = None;
            } else if !Path::new(p).is_absolute() {
                return Err(LibraryError::validation("recycle_bin_path must be absolute"));
            } else {
                self.check_recycle_bin(Path::new(p)).await?;
            }
        }
        s.extra_file_extensions = s
            .extra_file_extensions
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        if s.ffprobe_path.trim().is_empty() {
            s.ffprobe_path = "ffprobe".into();
        }
        self.db.settings().set(keys::MEDIA_MANAGEMENT, &s).await?;
        self.emit("settings", "updated", 0);
        Ok(s)
    }

    /// The recycle bin must not overlap a root folder of any kind (recycled files would show up
    /// as media, and purging the bin would delete the library) nor be or contain the data
    /// directory (the folder of the database file).
    async fn check_recycle_bin(&self, bin: &Path) -> Result<()> {
        if bin.components().any(|c| c == Component::ParentDir) {
            return Err(LibraryError::validation("recycle_bin_path must not contain `..`"));
        }
        let bin = resolve_lenient(bin);
        for r in content_roots::list(&self.db, &ContentKind::ALL).await? {
            let root = resolve_lenient(Path::new(&r.path));
            if bin.starts_with(&root) || root.starts_with(&bin) {
                return Err(LibraryError::validation(format!(
                    "recycle_bin_path must not overlap the root folder {}",
                    r.path
                )));
            }
        }
        let db_file: Option<String> =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
                .fetch_optional(self.db.pool())
                .await?;
        if let Some(data_dir) = db_file
            .as_deref()
            .filter(|f| !f.is_empty())
            .and_then(|f| Path::new(f).parent())
            .map(resolve_lenient)
            && data_dir.starts_with(&bin)
        {
            return Err(LibraryError::validation(
                "recycle_bin_path must not be or contain the data directory",
            ));
        }
        Ok(())
    }

    /// Guard over every root folder that exists on disk.
    pub(crate) async fn path_guard(&self) -> Result<PathGuard> {
        let mut g = PathGuard::default();
        for r in crate::repo::misc::root_folders(&self.db).await? {
            if Path::new(&r.path).is_dir() {
                g.add_root(Path::new(&r.path))?;
            }
        }
        Ok(g)
    }

    /// Deletes a file/folder: into the recycle bin when configured, else permanently.
    /// Missing paths are ignored. Returns the recycle-bin location, if any.
    pub(crate) async fn remove_path(&self, path: &Path) -> Result<Option<PathBuf>> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(None);
        }
        let guard = self.path_guard().await?;
        let mm = self.media_management().await?;
        match mm.recycle_bin_path.as_deref() {
            Some(bin) => {
                let retention = (mm.recycle_bin_retention_days > 0)
                    .then(|| Duration::from_secs(u64::from(mm.recycle_bin_retention_days) * 86_400));
                let bin = RecycleBin::new(bin, retention)?;
                Ok(Some(bin.recycle(path, &guard)?))
            }
            None => {
                delete_permanently(path, &guard)?;
                Ok(None)
            }
        }
    }
}

/// `path` with its deepest existing ancestor canonicalised (symlinks resolved).
fn resolve_lenient(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(base) = std::fs::canonicalize(cur) {
            return tail.iter().rev().fold(base, |acc, name| acc.join(name));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn naming_errors(s: &NamingSettings) -> Vec<NamingErrorDto> {
    let fields = [
        ("movie_folder_format", &s.movie_folder_format, true),
        ("movie_file_format", &s.movie_file_format, true),
        ("series_folder_format", &s.series_folder_format, true),
        ("season_folder_format", &s.season_folder_format, false),
        ("specials_folder_format", &s.specials_folder_format, false),
        ("standard_episode_format", &s.standard_episode_format, true),
        ("daily_episode_format", &s.daily_episode_format, true),
        ("anime_episode_format", &s.anime_episode_format, true),
    ];
    let mut out = Vec::new();
    for (field, tpl, required) in fields {
        if tpl.trim().is_empty() {
            if required {
                out.push(NamingErrorDto {
                    field: field.into(),
                    message: "must not be empty".into(),
                });
            }
            continue;
        }
        if let Err(e) = validate_template(tpl) {
            out.push(NamingErrorDto {
                field: field.into(),
                message: e.to_string(),
            });
        }
    }
    if s.max_file_name_bytes < 32 {
        out.push(NamingErrorDto {
            field: "max_file_name_bytes".into(),
            message: "must be at least 32".into(),
        });
    }
    out
}
