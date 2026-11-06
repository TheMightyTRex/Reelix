//! One-shot commands on a data directory without starting the server (`reelix backup create`,
//! `restore check|stage`, `migrate prepare|list`, `config relocate`). The Windows installer uses
//! them to prepare a recovery or migration before Reelix first starts; they are also handy for
//! automation. Each prints a short result on stdout, or the error on stderr with exit code 1.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;
use chrono::Utc;
use reelix_api_types::migrate::SourceKind;
use reelix_api_types::system::BackupKind;
use reelix_db::Db;

use crate::cli::{BackupAction, ConfigAction, MigrateAction, RestoreAction};
use crate::config::Config;
use crate::maintenance::{self, Paths, backup, server_settings};

/// The maintenance commands; anything else isn't handled here.
pub enum Maintenance<'a> {
    Backup(&'a BackupAction),
    Restore(&'a RestoreAction),
    Migrate(&'a MigrateAction),
    Config(&'a ConfigAction),
}

pub fn main(command: Maintenance<'_>, config: &Config) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("reelix: failed to start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(command, config)) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("reelix: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs `command`; the `Ok` text is what to print.
pub async fn run(command: Maintenance<'_>, config: &Config) -> anyhow::Result<String> {
    let paths = Paths::from_config(config);
    match command {
        Maintenance::Backup(BackupAction::Create) => {
            if paths.db_file.is_none() {
                anyhow::bail!("backups are only supported for SQLite databases");
            }
            let db = open_db(config).await?;
            let result = maintenance::snapshot_backup(&db, &paths, BackupKind::Manual).await;
            db.pool().close().await;
            Ok(result?.display().to_string())
        }
        Maintenance::Restore(RestoreAction::Check { archive }) => {
            backup::validate(archive).with_context(|| invalid(archive))?;
            Ok(format!("{} is a valid Reelix backup", archive.display()))
        }
        Maintenance::Restore(RestoreAction::Stage { archive }) => {
            if paths.db_file.is_none() {
                anyhow::bail!("restoring backups is only supported for SQLite databases");
            }
            let source = archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| backup::is_safe_name(n))
                .unwrap_or_else(|| "upload".into());
            backup::stage(&paths, archive, &source, Utc::now()).with_context(|| invalid(archive))?;
            Ok(format!(
                "staged {}; Reelix restores it the next time it starts",
                archive.display()
            ))
        }
        Maintenance::Migrate(MigrateAction::Prepare {
            source,
            url,
            api_key_file,
        }) => prepare_migration(config, *source, url, api_key_file).await,
        Maintenance::Migrate(MigrateAction::List) => {
            let db = open_db(config).await?;
            let list = reelix_migrate::pending::list(&db).await;
            db.pool().close().await;
            Ok(serde_json::to_string_pretty(&list?)?)
        }
        Maintenance::Config(ConfigAction::Relocate { from }) => {
            let changed = server_settings::relocate(&paths.config_file, from, &config.data_dir)?;
            Ok(if changed.is_empty() {
                format!("{}: nothing to change", paths.config_file.display())
            } else {
                format!("{}: updated {}", paths.config_file.display(), changed.join(", "))
            })
        }
    }
}

fn invalid(archive: &Path) -> String {
    format!("{} is not a valid Reelix backup", archive.display())
}

/// Opens (creating and migrating, like the server does) the configured database.
async fn open_db(config: &Config) -> anyhow::Result<Db> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating directory {}", config.data_dir.display()))?;
    Db::connect(&config.database_url)
        .await
        .context("opening database")
}

async fn prepare_migration(
    config: &Config,
    source: SourceKind,
    url: &str,
    key_file: &Path,
) -> anyhow::Result<String> {
    let key = std::fs::read_to_string(key_file)
        .with_context(|| format!("reading the API key from {}", key_file.display()))?;
    let key = key.trim().trim_start_matches('\u{feff}');
    if key.is_empty() {
        anyhow::bail!("{} is empty", key_file.display());
    }
    let db = open_db(config).await?;
    let result = async {
        let secrets = crate::server::load_secrets(config)?;
        reelix_migrate::pending::add(&db, &secrets, source, url, key)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
    .await;
    db.pool().close().await;
    let saved = result?;
    Ok(format!(
        "saved {} at {}; review it in System → Migrate after setup",
        source.label(),
        saved.url
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use reelix_api_types::migrate::{MigrateConnection, SAVED_API_KEY};
    use reelix_db::secrets::SecretBox;

    use super::*;
    use crate::config::CliOverrides;

    fn config(data_dir: &Path) -> Config {
        Config::load(&CliOverrides {
            data_dir: Some(data_dir.to_path_buf()),
            ..Default::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn backup_create_then_restore_stage_and_apply() {
        let tmp = tempfile::tempdir().unwrap();
        let src = config(&tmp.path().join("src"));
        let db = open_db(&src).await.unwrap();
        db.settings().set("marker", "from-backup").await.unwrap();
        db.pool().close().await;

        let out = run(Maintenance::Backup(&BackupAction::Create), &src)
            .await
            .unwrap();
        let zip = PathBuf::from(out);
        assert!(zip.starts_with(&src.backup_dir), "{}", zip.display());
        let name = zip.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("reelix_backup_manual_"), "{name}");

        let check = RestoreAction::Check { archive: zip.clone() };
        run(Maintenance::Restore(&check), &src).await.unwrap();

        // Stage into a data folder that has no database yet (the installer's case).
        let dst = config(&tmp.path().join("dst"));
        let stage = RestoreAction::Stage { archive: zip.clone() };
        run(Maintenance::Restore(&stage), &dst).await.unwrap();
        let dst_paths = Paths::from_config(&dst);
        let pending = backup::pending(&dst_paths).unwrap();
        assert_eq!(pending.source, name);
        assert!(dst.data_dir.join("restore").join("pending.zip").is_file());
        assert!(zip.is_file(), "the original archive is copied, not moved");

        // What the server does on its next start.
        assert!(
            backup::apply_pending_restore(&dst_paths, &dst.database_url)
                .await
                .unwrap()
        );
        let db = Db::connect(&dst.database_url).await.unwrap();
        assert_eq!(
            db.settings().get::<String>("marker").await.unwrap().as_deref(),
            Some("from-backup")
        );
        db.pool().close().await;
    }

    #[tokio::test]
    async fn invalid_backups_are_refused_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(&tmp.path().join("data"));
        let junk = tmp.path().join("reelix_backup_manual_2026.01.01_00.00.00.zip");
        std::fs::write(&junk, b"not a zip").unwrap();
        for action in [
            RestoreAction::Check {
                archive: junk.clone(),
            },
            RestoreAction::Stage {
                archive: junk.clone(),
            },
        ] {
            let err = run(Maintenance::Restore(&action), &cfg).await.unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("is not a valid Reelix backup") && msg.contains("not a zip file"),
                "{msg}"
            );
        }
        let missing = RestoreAction::Stage {
            archive: tmp.path().join("missing.zip"),
        };
        assert!(run(Maintenance::Restore(&missing), &cfg).await.is_err());
        assert!(backup::pending(&Paths::from_config(&cfg)).is_none());
    }

    #[tokio::test]
    async fn migrate_prepare_saves_an_encrypted_key_and_lists_it_masked() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(&tmp.path().join("data"));
        let key = "fedcba9876543210fedcba9876543210";
        let key_file = tmp.path().join("key.txt");
        std::fs::write(&key_file, format!("{key}\r\n")).unwrap();
        let prepare = MigrateAction::Prepare {
            source: SourceKind::Radarr,
            url: "http://localhost:7878/".into(),
            api_key_file: key_file.clone(),
        };
        // No database or secret key yet: both are created.
        let out = run(Maintenance::Migrate(&prepare), &cfg).await.unwrap();
        assert!(out.contains("http://localhost:7878"), "{out}");
        assert!(cfg.secret_key_file.is_file());

        let listed = run(Maintenance::Migrate(&MigrateAction::List), &cfg)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
        assert_eq!(listed[0]["source"], "radarr");
        assert_eq!(listed[0]["url"], "http://localhost:7878");
        assert_eq!(listed[0]["api_key"], SAVED_API_KEY);
        assert!(!listed.to_string().contains(key));

        let db = Db::connect(&cfg.database_url).await.unwrap();
        let raw: serde_json::Value = db
            .settings()
            .get(reelix_migrate::pending::SETTINGS_KEY)
            .await
            .unwrap()
            .unwrap();
        assert!(!raw.to_string().contains(key), "{raw}");
        // The server, with the same secret key, gets the key back.
        let secrets = SecretBox::load_or_create(&cfg.secret_key_file).unwrap();
        let mut c = MigrateConnection {
            source: SourceKind::Radarr,
            url: "http://localhost:7878".into(),
            api_key: SAVED_API_KEY.into(),
            path_rewrites: Vec::new(),
            secrets: Default::default(),
        };
        assert!(
            reelix_migrate::pending::resolve_key(&db, &secrets, &mut c)
                .await
                .unwrap()
        );
        assert_eq!(c.api_key, key);
        db.pool().close().await;

        // An empty key file or a bad URL is refused.
        std::fs::write(&key_file, "  \n").unwrap();
        assert!(run(Maintenance::Migrate(&prepare), &cfg).await.is_err());
        std::fs::write(&key_file, key).unwrap();
        let bad = MigrateAction::Prepare {
            source: SourceKind::Sonarr,
            url: "localhost:8989".into(),
            api_key_file: key_file,
        };
        assert!(run(Maintenance::Migrate(&bad), &cfg).await.is_err());
    }

    #[tokio::test]
    async fn config_relocate_points_the_copied_config_at_the_new_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old").join("data");
        let new = tmp.path().join("new");
        std::fs::create_dir_all(&new).unwrap();
        let o = old.to_string_lossy().replace('\\', "/");
        std::fs::write(
            new.join("config.toml"),
            format!("[paths]\nlog_dir = \"{o}/logs\"\n"),
        )
        .unwrap();
        let cfg = config(&new);
        let relocate = ConfigAction::Relocate { from: old };
        let out = run(Maintenance::Config(&relocate), &cfg).await.unwrap();
        assert!(out.contains("paths.log_dir"), "{out}");
        let cfg = config(&new);
        assert_eq!(cfg.log_dir, new.join("logs"));
        let out = run(Maintenance::Config(&relocate), &cfg).await.unwrap();
        assert!(out.contains("nothing to change"), "{out}");
    }
}
